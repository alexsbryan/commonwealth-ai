// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn seat watch` — the notes-rail poller as a mechanism (order
//! `commons-fluency` fix 8).
//!
//! The 2026-08-12 drill ran its cross-machine watchers by hand as
//! background pollers; this verb turns that pattern into a mechanism.
//! It polls the daemon's notes rail for records ADDRESSED to this
//! node's seat — any note carrying a `related_entity` anchor (the
//! seat's coordination rail: comaintainer-seat, order-seat,
//! directive-log, and any anchor the open registry
//! `quality/operational-anchors.toml` later gains) — and surfaces each
//! new one as a `SEAT_WATCH` line on stdout, one record per line, so a
//! session-level background monitor (the pattern the drill proved by
//! hand) can turn them into notifications.
//!
//! The seat opt-in is `include_operational: true` — the same flag the
//! seat's own ambient read uses (UC-D4 inverse). Ordinary sessions
//! never see these rows; the watcher is explicitly a seat instrument.
//!
//! Latency is measured, never guessed: each surfaced record prints the
//! fix-3 receipts — `sent_at` (origin publish clock) and `received_at`
//! (this node's first local observation) — and the watcher's OWN lag
//! (`now - received_at`) when the local receipt exists. A record with
//! no receipt prints `-`; absence is reported, never defaulted
//! (ARCH §18.3).
//!
//! Honest failure: a daemon that is down or rejects the seat read
//! prints a named line once per failure streak and KEEPS POLLING —
//! every case verdict the drill draws from a silent watcher would be
//! could-not-judge, never a hang.
//!
//! Session-level is enough by order: a seat that is not running is
//! honestly reported by the drill as no-peer at the case deadline. A
//! durable daemon-side inbox is a FUTURE order, named here so nobody
//! builds it accidentally.

use std::collections::HashSet;

/// Default poll cadence, seconds. Matches the daemon's inbound ingest
/// poller so a surfaced record is at most one cadence late.
const DEFAULT_POLL_EVERY_SECS: u64 = 10;

/// Max notes examined per poll. Mirrors the CLI's `notes list` cap
/// (limit.min(100)) — the daemon-side cap is the same number.
const DEFAULT_POLL_LIMIT: u64 = 100;

/// The seat's rail by default: the OPERATIONAL RECORD anchors from
/// `quality/operational-anchors.toml` (mirrored in read_notes.rs's
/// compiled floor). `related_entity` is a general anchor column — the
/// commit harvester writes commit SHAs there — so the watcher must
/// NOT default to "any anchored record"; that would surface every
/// harvested commit as if it were addressed to the seat. The mirror
/// test `default_anchors_mirror_the_registry_file` pins this list to
/// the file; append here when the registry gains an anchor.
const DEFAULT_WATCH_ANCHORS: [&str; 3] = ["comaintainer-seat", "order-seat", "directive-log"];

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One seat-addressed record as the watcher surfaced it.
struct Sighting {
    id: String,
    kind: String,
    anchor: String,
    sent_at: Option<i64>,
    received_at: Option<i64>,
    content_first_line: String,
}

/// Extract the seat-addressed records from a daemon `notes` payload,
/// filtering to rows whose `related_entity` matches the anchor set.
/// `anchors` empty = every anchored record is addressed (the open
/// registry's whole point: a new anchor needs no CLI change).
fn sightings_from_payload(payload: &serde_json::Value, anchors: &[String]) -> Vec<Sighting> {
    payload
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| {
                    let anchor = n.get("related_entity")?.as_str()?.to_string();
                    if !anchors.is_empty() && !anchors.iter().any(|a| a == &anchor) {
                        return None;
                    }
                    let id = n
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let kind = n
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let content = n.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let first_line = content
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(80)
                        .collect::<String>();
                    Some(Sighting {
                        id,
                        kind,
                        anchor,
                        sent_at: n.get("sent_at").and_then(|v| v.as_i64()),
                        received_at: n.get("received_at").and_then(|v| v.as_i64()),
                        content_first_line: first_line,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Write one line to stdout, tolerating a closed pipe. A poller is
/// meant to be consumed by `head` / `grep -q` — the F-drill's watch
/// probe pipes `seat watch --once | grep -q` — and Rust ignores
/// SIGPIPE, so a raw `println!` panics ("failed printing to stdout:
/// Broken pipe") the moment the consumer closes the pipe (observed
/// 2026-08-12: `seat watch --once | head -3` exited 101).
fn say(line: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

/// Print one surfaced record as a single stdout line — the event
/// stream a session-level background monitor consumes.
fn print_sighting(s: &Sighting) {
    let sent = s
        .sent_at
        .map(|t| t.to_string())
        .unwrap_or_else(|| "-".into());
    let received = s
        .received_at
        .map(|t| t.to_string())
        .unwrap_or_else(|| "-".into());
    let lag = s
        .received_at
        .map(|r| format!(" lag={}s", (unix_now() - r).max(0)))
        .unwrap_or_default();
    say(&format!(
        "SEAT_WATCH {} kind={} anchor={} sent={} received={}{} {}",
        s.id, s.kind, s.anchor, sent, received, lag, s.content_first_line
    ));
}

/// `svrn seat watch` — poll the daemon's notes rail for seat-addressed
/// records and surface new ones. Runs until interrupted, or one poll
/// with `--once`.
pub async fn run(args: &[String]) -> i32 {
    // Subcommand gate: the only subcommand today is `watch` (the order
    // names the verb `sovereign seat watch`). Anything else is named,
    // never silently treated as a poll.
    match args.first().map(String::as_str) {
        None | Some("watch") => {}
        Some("help") | Some("--help") | Some("-h") => {
            println!(
                "Usage: svrn seat watch [--every SECS] [--limit N] [--anchors a,b,c] [--once]\n\
                 \n\
                 Poll the daemon's notes rail for records addressed to this node's seat\n\
                 (any related_entity anchor — the seat's coordination rail) and print each\n\
                 new one as a SEAT_WATCH line with its fix-3 receipts (sent_at, received_at)\n\
                 and the watcher's own lag. Session-level background monitor: run it in the\n\
                 background and read its stdout lines as events. --once polls once and exits.\n\
                 \n\
                 A durable daemon-side inbox is deliberately NOT built (a future order —\n\
                 see the order commons-fluency fix 8 text)."
            );
            return 0;
        }
        Some(other) => {
            eprintln!("seat: unknown subcommand {other:?} — the only subcommand is `watch`");
            return 2;
        }
    }
    let mut every = DEFAULT_POLL_EVERY_SECS;
    let mut once = false;
    let mut limit = DEFAULT_POLL_LIMIT;
    let mut anchors: Vec<String> = DEFAULT_WATCH_ANCHORS
        .iter()
        .map(|s| s.to_string())
        .collect();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--every" => {
                every = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or_else(|| {
                        eprintln!("seat watch: --every must be a positive number of seconds");
                        DEFAULT_POLL_EVERY_SECS
                    });
                i += 2;
            }
            "--limit" => {
                limit = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .filter(|v| *v > 0)
                    .unwrap_or_else(|| {
                        eprintln!("seat watch: --limit must be a positive number");
                        DEFAULT_POLL_LIMIT
                    })
                    .min(DEFAULT_POLL_LIMIT);
                i += 2;
            }
            "--anchors" => {
                anchors = args
                    .get(i + 1)
                    .map(|v| {
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                i += 2;
            }
            "--once" => {
                once = true;
                i += 1;
            }
            other => {
                eprintln!("seat watch: unknown flag {other} (see `svrn seat --help`)");
                return 2;
            }
        }
    }

    {
        // Ungated since order commons-fluency item 11: the daemon path
        // is CORE seat behavior. A code-intel cfg gate here compiled
        // the refusal branch into every deployed build (dev-build.sh's
        // treesitter + dev-tools contract and the four-sibling release
        // carry no code-intel), so `seat watch`
        // answered the daemon-access refusal on both sides of all three
        // F-drill runs — the second instance of the DaemonFirst
        // disease. `mcp_client` is available to this crate by workspace
        // unification (sovereign-cli-dev / sovereign-cli-llm enable
        // sovereign-cli-shared/mcp-client unconditionally), so the
        // route carries in every contract; the pin test
        // seat_watch_daemon_path_is_never_feature_gated bans any
        // re-gate.
        use sovereign_cli_shared::mcp_client::{daemon_tool_call, DaemonCallError};

        let anchor_desc = if anchors.is_empty() {
            "any anchored record (open registry)".to_string()
        } else {
            anchors.join(", ")
        };
        say(&format!(
            "seat watch: polling the daemon notes rail every {every}s for: {anchor_desc} \
             (include_operational — the seat opt-in)"
        ));

        // quiet_polls counts consecutive polls with nothing new, so the
        // heartbeat is ~1/minute at the default cadence, not one line per
        // poll — a healthy quiet channel must not flood the event stream,
        // and a dead one must still be distinguishable from silence.
        let mut quiet_polls: u64 = 0;
        async fn poll(
            seen: &mut HashSet<String>,
            anchors: &[String],
            limit: u64,
            quiet_polls: &mut u64,
        ) -> i32 {
            let mut args = serde_json::Map::new();
            args.insert("include_operational".into(), serde_json::json!(true));
            args.insert("limit".into(), serde_json::json!(limit.min(100)));
            match daemon_tool_call("notes", serde_json::Value::Object(args)).await {
                Ok(payload) => {
                    let mut new_count = 0usize;
                    for s in sightings_from_payload(&payload, anchors) {
                        if seen.insert(s.id.clone()) {
                            print_sighting(&s);
                            new_count += 1;
                        }
                    }
                    if new_count == 0 {
                        *quiet_polls += 1;
                        if *quiet_polls % 6 == 0 {
                            say("seat watch: heartbeat — poll ok, no new seat-addressed records");
                        }
                    } else {
                        *quiet_polls = 0;
                    }
                    0
                }
                Err(DaemonCallError::Unreachable(_)) => {
                    *quiet_polls = 0;
                    say("seat watch: daemon unreachable — could-not-judge while down, will keep polling");
                    1
                }
                Err(DaemonCallError::Tool(msg)) => {
                    *quiet_polls = 0;
                    say(&format!(
                        "seat watch: daemon rejected the seat read: {msg} — could-not-judge, \
                         will keep polling"
                    ));
                    1
                }
            }
        }

        let mut seen: HashSet<String> = HashSet::new();
        // First poll surfaces EVERYTHING addressed — the drill's start
        // note must be readable by a watcher that began after it was
        // written (the bootstrap honesty clause, order §UC-F8).
        poll(&mut seen, &anchors, limit, &mut quiet_polls).await;
        if once {
            return 0;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(every)).await;
            poll(&mut seen, &anchors, limit, &mut quiet_polls).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seat verb's daemon path must NEVER be feature-gated again
    /// (order commons-fluency, item 11 — the second instance of the
    /// DaemonFirst disease): the deployed builds (dev-build.sh's
    /// treesitter + dev-tools contract; the four-sibling release)
    /// carry no `code-intel`, so a gated path compiled the refusal
    /// branch — `sovereign seat watch --once` answered "no daemon
    /// access" on both sides of all three F-drill runs. A runtime
    /// refusal is invisible to the lint and test gates, which build
    /// WITH code-intel (where a re-gated refusal branch is cfg'd out
    /// and the string hides in source), so this test pins the source
    /// itself, build-independent: the refusal string is banned and
    /// this file carries zero code-intel cfgs. The daemon path is
    /// unconditional — mcp_client reaches this crate by workspace
    /// unification (sovereign-cli-dev / sovereign-cli-llm enable
    /// sovereign-cli-shared/mcp-client unconditionally), and a solo
    /// `-p sovereign-cli` build that lacks it fails LOUDLY at compile
    /// time rather than shipping a silent runtime refusal.
    #[test]
    fn seat_watch_daemon_path_is_never_feature_gated() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/seat_cmd.rs"))
            .expect("seat_cmd.rs source must be readable (repo layout)");
        // The needles are assembled from parts so this test's own
        // literals never form the contiguous strings it bans — a
        // self-referential assert can never pass (the first run of
        // this very test proved it: the refusal string lived in the
        // failure message and the narrative comment, both caught).
        const REFUSAL: &str = concat!("no daemon ", "access");
        const CFG_ON: &str = concat!("#[cfg(feature = \"code-", "intel\")]");
        const CFG_OFF: &str = concat!("#[cfg(not(feature = \"code-", "intel\"))]");
        assert!(
            !src.contains(REFUSAL),
            "the seat watch daemon-refusal string must not exist in source — \
             it shipped in every dev-tools-only build (item 11)"
        );
        assert!(
            !src.contains(CFG_ON),
            "seat_cmd.rs must carry no code-intel cfg — the seat verb's \
             daemon path is core behavior, not a code-intel feature"
        );
        assert!(
            !src.contains(CFG_OFF),
            "seat_cmd.rs must carry no negative code-intel cfg — the \
             refusal branch is banned outright"
        );
    }

    /// The default anchor list is the seat's rail, and the rail is the
    /// open registry file — the same mirror contract read_notes.rs
    /// holds for its compiled floor (one direction each; the two
    /// mirror tests together pin all three copies).
    #[test]
    fn default_anchors_mirror_the_registry_file() {
        let registry = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../quality/operational-anchors.toml"
        );
        let text = std::fs::read_to_string(registry)
            .expect("quality/operational-anchors.toml must exist (repo layout)");
        for name in DEFAULT_WATCH_ANCHORS {
            assert!(
                text.contains(&format!("name = \"{name}\"")),
                "anchor {name} is a seat-rail default but is not in the registry file"
            );
        }
    }

    /// Sighting extraction filters to addressed records: anchored rows
    /// matching the set survive; commit-SHA anchors (the harvest
    /// writer's related_entity) do not unless explicitly requested.
    #[test]
    fn sightings_filter_to_requested_anchors() {
        let payload = serde_json::json!({
            "notes": [
                {"id": "seat-1", "kind": "decision", "content": "to the seat",
                 "related_entity": "comaintainer-seat", "sent_at": 100, "received_at": 105},
                {"id": "sha-1", "kind": "decision", "content": "harvested commit",
                 "related_entity": "3bb1947881e6851c79bdf037a7c43e57383598aa"},
                {"id": "none-1", "kind": "decision", "content": "ordinary knowledge",
                 "related_entity": null}
            ]
        });
        let anchors: Vec<String> = vec!["comaintainer-seat".into()];
        let out = sightings_from_payload(&payload, &anchors);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "seat-1");
        assert_eq!(out[0].sent_at, Some(100));
        assert_eq!(out[0].received_at, Some(105));
        // Explicit --anchors opts the SHA anchor in.
        let sha: Vec<String> = vec!["3bb1947881e6851c79bdf037a7c43e57383598aa".into()];
        let out = sightings_from_payload(&payload, &sha);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "sha-1");
    }
}
