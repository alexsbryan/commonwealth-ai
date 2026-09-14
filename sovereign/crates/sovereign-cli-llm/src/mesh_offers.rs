// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh offers` — what the neighbours have going spare, with the ones
//! that did not answer NAMED.
//!
//! # The sentence this verb exists to say
//!
//! Every marketplace shows you a full grid and none of them can tell you what
//! is missing from it. This one can, because the catalogue is COMPUTED: every
//! neighbour is asked at once and every neighbour is a ROW — served, failed,
//! or never asked and why. There is no stored listing to be excluded from and
//! nobody positioned to rank (`docs/internal/RING_APPLICATIONS.md` §Commerce).
//!
//! `--why` says the second sentence no marketplace can: who vouched for this
//! seller, on what act, and when. It is the first surface that reads
//! `Roster.vouches` for somebody other than the operator.
//!
//! # What this verb refuses to do
//!
//! **It does not merge, dedup, rank or schematise what a seller answered.**
//! `commonwealth_media::fanout`'s module docs refuse that on the grounds that
//! item semantics are the origin's and a shim speaking one seller's schema
//! merges better than this repository could. So a served row prints the
//! origin's OWN bytes, indented and capped, and counts nothing about them —
//! except, when the answer is a JSON array, how many elements the array has,
//! which is a fact about the document rather than a claim about what an item
//! is.
//!
//! That is a deliberate departure from the shape the order sketched (a per-
//! item line, `drill — lend, back by Sunday`), which cannot be produced
//! without deciding what an item's fields are called. The order's own seam
//! outranks its example, and it says so: "the exact rendering is yours; the
//! three properties are not."
//!
//! # Why `peers` is always sent
//!
//! With `peers` absent the fanout targets only members that ADVERTISE the
//! kind — so a neighbour publishing no offer origin is not in the answer at
//! all. That absence is the whole thing this rung exists to prevent, so the
//! verb enumerates the roster itself and names every active member. Self is
//! excluded: `origin_fanout` never asks this node, and a catalogue of one's
//! own shelf is not a catalogue.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::mesh_cmd::daemon_client_port;

/// The origin-relative path asked of every offer origin when the caller names
/// none. The root, because an offer origin is the operator's own listing
/// server and this repository has no business inventing a path convention for
/// it — `/` is the one path every HTTP server has an opinion about.
const DEFAULT_OFFER_PATH: &str = "/";

/// How much of a seller's own answer to show under its row.
const BODY_PREVIEW_BYTES: usize = 600;

pub(crate) async fn cmd_offers(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        print_help();
        return 0;
    }
    let json_out = args.iter().any(|a| a == "--json");
    let why = args.iter().any(|a| a == "--why");
    let who = args.iter().any(|a| a == "--who");
    let mut path = DEFAULT_OFFER_PATH.to_string();
    let mut peer: Option<String> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" | "--why" | "--who" => {}
            "--path" => {
                i += 1;
                match args.get(i) {
                    Some(p) => path = p.clone(),
                    None => {
                        eprintln!("--path wants an origin-relative path, e.g. --path /items");
                        return 2;
                    }
                }
            }
            "--timeout-ms" => {
                i += 1;
                // Refused by name rather than `.parse().ok()`. A typo'd cap
                // silently becoming the 10s default is a request that did not
                // do what was asked and said nothing — and the person who
                // typed `--timeout-ms 2s` would read the slow run as the
                // mesh being slow (ARCH §18.3).
                match args.get(i).map(|v| v.parse::<u64>()) {
                    Some(Ok(ms)) => timeout_ms = Some(ms),
                    Some(Err(e)) => {
                        eprintln!(
                            "--timeout-ms {:?} is not a whole number of milliseconds ({e})",
                            args[i]
                        );
                        return 2;
                    }
                    None => {
                        eprintln!("--timeout-ms wants a number of milliseconds, e.g. 2000");
                        return 2;
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("unknown flag {other} — `svrn mesh offers --help`");
                return 2;
            }
            other => peer = Some(other.to_string()),
        }
        i += 1;
    }

    let port = daemon_client_port();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };

    if let Some(peer) = peer {
        return reach_one(&client, port, &peer, json_out).await;
    }
    if who {
        return list_publishers(&client, port, json_out).await;
    }
    catalogue(&client, port, &path, timeout_ms, why, json_out).await
}

fn print_help() {
    eprintln!("Usage: svrn mesh offers [--why] [--path P] [--timeout-ms N] [--json]");
    eprintln!("       svrn mesh offers --who        # who publishes an offer origin (gossip only)");
    eprintln!(
        "       svrn mesh offers <peer>       # a localhost URL to ONE member's offer origin"
    );
    eprintln!();
    eprintln!("Ask every neighbour at once what it has for sale or lending, and print one row");
    eprintln!("each — what its origin answered, or why it was not asked. A neighbour that is");
    eprintln!("offline, or publishes no offer origin, is a ROW carrying its reason, never an");
    eprintln!("absence. That is the part a marketplace cannot show you.");
    eprintln!();
    eprintln!("You hold no credential of anybody's. Each request rides your own mesh key and");
    eprintln!("the holder's daemon tells its origin who is asking.");
    eprintln!();
    eprintln!("Nothing is merged, ranked or deduplicated: an offer means whatever the seller's");
    eprintln!("origin says it means, and each row prints that origin's own answer.");
    eprintln!();
    eprintln!("To publish yours, point the key at any HTTP server on loopback:");
    eprintln!("  [iroh]");
    eprintln!("  offer_origin = \"127.0.0.1:8710\"");
    eprintln!("  offer_allow  = [\"Mira\", \"node-44ae7614\"]   # optional; empty = every member");
    eprintln!("then `svrn daemon stop && svrn daemon start` (a reload cannot: the acceptor");
    eprintln!("reads this once, while it is built).");
    eprintln!();
    eprintln!("Flags:");
    eprintln!("  --why           Add who vouched for each seller: the person, the introduction");
    eprintln!("                  op and its date, from the rings THIS node holds. A seller no");
    eprintln!("                  ring of yours claims reads `warrant unknown` — never a guess.");
    eprintln!("  --who           List the members that advertise an offer origin, from gossip.");
    eprintln!("                  Nothing is dialed, so nothing is asked and nothing answers.");
    eprintln!("  --path P        Ask each origin for P instead of `/`.");
    eprintln!("  --timeout-ms N  Per-member cap (default 10000); a slower one is a failed row.");
    eprintln!("  --json          The daemon's own document: path, kind, asked, rows[].");
}

// ─── the catalogue ──────────────────────────────────────────────────────────

/// One member of this node's mesh, as the catalogue needs it.
struct Neighbour {
    name: String,
    node_id: String,
    /// The hex Ed25519 key the mesh gossips. The ONLY join key `--why` uses —
    /// see [`warrant_of`].
    pubkey: Option<String>,
}

/// Every active member other than this node, from `GET /v1/mesh/status`.
///
/// Read here rather than left to the fanout's own selection because the
/// fanout with no `peers` targets only members that ADVERTISE the kind, and a
/// neighbour publishing none would then be absent instead of a row.
async fn neighbours(client: &reqwest::Client, port: u16) -> Result<Vec<Neighbour>, String> {
    let url = format!("http://127.0.0.1:{port}/v1/mesh/status");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("daemon at {url} not reachable: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status()));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{url} did not parse: {e}"))?;
    let rows = v
        .get("members")
        .and_then(|m| m.as_array())
        .ok_or_else(|| format!("{url} carried no `members` — this CLI and that daemon do not agree on the shape of /v1/mesh/status"))?;
    let live: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|m| m.get("is_self").and_then(|b| b.as_bool()) != Some(true))
        // A tombstoned member is not a neighbour who did not answer; it is
        // somebody who left. Naming it would be a row about nobody.
        .filter(|m| m.get("active").and_then(|b| b.as_bool()) != Some(false))
        .collect();
    let neighbours: Vec<Neighbour> = live
        .iter()
        .filter_map(|m| {
            Some(Neighbour {
                name: m.get("name")?.as_str()?.to_string(),
                node_id: m.get("node_id")?.as_str()?.to_string(),
                pubkey: m
                    .get("node_pubkey")
                    .and_then(|k| k.as_str())
                    .map(|k| k.to_ascii_lowercase()),
            })
        })
        .collect();
    // A member row this build cannot read is REPORTED, never quietly dropped.
    // A dropped one is exactly the absence this whole verb exists to prevent,
    // and it would be invisible: the grid would simply be one narrower
    // (ARCH §18.3).
    if neighbours.len() != live.len() {
        return Err(format!(
            "{} of {} member rows carry no readable `name`/`node_id` — this CLI and that \
             daemon do not agree on the shape of /v1/mesh/status, and a catalogue built \
             from the rest would be silently short",
            live.len() - neighbours.len(),
            live.len()
        ));
    }
    Ok(neighbours)
}

async fn catalogue(
    client: &reqwest::Client,
    port: u16,
    path: &str,
    timeout_ms: Option<u64>,
    why: bool,
    json_out: bool,
) -> i32 {
    let neighbours = match neighbours(client, port).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("mesh offers: {e}");
            eprintln!("The roster lives in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    if neighbours.is_empty() {
        println!(
            "No neighbours. This node is alone on its mesh, and a catalogue comes from \
             peers — `origin_fanout` never asks this node, so there is nothing to ask.\n\
             Invite somebody:  svrn mesh rotate"
        );
        return 0;
    }
    let names: Vec<&str> = neighbours.iter().map(|n| n.name.as_str()).collect();

    let url = format!("http://127.0.0.1:{port}/v1/mesh/fanout");
    let mut body = serde_json::json!({
        "path": path,
        "kind": "offer",
        "peers": names,
    });
    if let Some(t) = timeout_ms {
        body["timeout_ms"] = serde_json::json!(t);
    }
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh offers: daemon at {url} not reachable: {e}");
            eprintln!("The bridges live in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        eprint!(
            "{}",
            crate::mesh_skew::render_kind_refusal(
                client,
                port,
                "POST /v1/mesh/fanout",
                "offer",
                status,
                text,
            )
            .await
        );
        return 1;
    }
    if json_out {
        println!("{text}");
        return 0;
    }
    let doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mesh offers: response shape mismatch ({e}): {text}");
            return 1;
        }
    };
    let rows = doc["rows"].as_array().cloned().unwrap_or_default();
    let warrants = if why {
        match warrants_for(&neighbours).await {
            Ok(w) => Some(w),
            Err(e) => {
                // COULD-NOT-JUDGE is not "no warrant" (ARCH §18.3). The rows
                // still print; the reader is told the join did not run.
                eprintln!(
                    "NOTE: the warrants could not be read, so no row below carries one: {e}\n"
                );
                Some(BTreeMap::new())
            }
        }
    } else {
        None
    };

    println!(
        "What the neighbours have going spare — {} asked for {}",
        doc["asked"].as_u64().unwrap_or(rows.len() as u64),
        doc["path"].as_str().unwrap_or(path)
    );
    println!();
    for r in &rows {
        let name = r["name"].as_str().unwrap_or("?");
        let ms = r["elapsed_ms"].as_u64().unwrap_or(0);
        match r["verdict"].as_str().unwrap_or("?") {
            "served" => {
                let entries = match &r["json"] {
                    serde_json::Value::Array(a) => format!("  {} entries", a.len()),
                    _ => String::new(),
                };
                println!(
                    "  {name:<16} {}  {} B{}{entries}   {ms} ms",
                    r["status"].as_u64().unwrap_or(0),
                    r["bytes"].as_u64().unwrap_or(0),
                    if r["truncated"].as_bool().unwrap_or(false) {
                        " (cut)"
                    } else {
                        ""
                    },
                );
                for line in preview(r["body"].as_str().unwrap_or("")) {
                    println!("      {line}");
                }
            }
            "failed" => println!(
                "  {name:<16} failed after {ms} ms\n      {}",
                r["reason"].as_str().unwrap_or("")
            ),
            "never_asked" => println!(
                "  {name:<16} not asked\n      {}",
                r["reason"].as_str().unwrap_or("")
            ),
            other => println!("  {name:<16} {other}"),
        }
        if let Some(w) = &warrants {
            println!(
                "      vouch: {}",
                w.get(name).map(String::as_str).unwrap_or(UNCLAIMED)
            );
        }
    }
    println!();
    println!(
        "Nothing above is merged, ranked or deduplicated — each row is that origin's own\n\
         answer, and what an offer MEANS is the seller's to say. A row that says `not asked`\n\
         is a neighbour this catalogue could not reach, named."
    );
    0
}

/// The seller's own bytes, indented and capped. Never parsed into fields:
/// what an item IS belongs to the origin (`commonwealth_media::fanout`).
fn preview(body: &str) -> Vec<String> {
    let cut = body.len() > BODY_PREVIEW_BYTES;
    let head: String = body.chars().take(BODY_PREVIEW_BYTES).collect();
    let mut out: Vec<String> = head.lines().map(str::to_string).collect();
    if cut {
        out.push("… (cut; `--json` for the whole answer)".to_string());
    }
    out
}

// ─── the vouch join ─────────────────────────────────────────────────────────

/// What a row says when no ring this node holds claims the seller's key.
///
/// Worded as an ABSENCE OF EVIDENCE HERE and not as a judgment about the
/// seller: this node simply has no local binding for that key. That is the
/// SPKI/SDSI reading — a name resolves by following YOUR OWN local bindings,
/// and having none is a fact about you.
const UNCLAIMED: &str = "warrant unknown — no ring on this node claims that key";

/// What a row says when the mesh itself never learned the seller's key.
const NO_KEY: &str = "warrant unknown — this member gossips no key, so there is nothing to \
                      resolve (a daemon older than mesh identity)";

/// One rendered warrant per neighbour NAME, for the rows to print under.
///
/// Asks the daemon for each ring's roster and admission — the same read
/// `svrn ring roster show` makes, through
/// [`sovereign_cli_shared::rail::roster_and_admission`], so the catalogue and
/// the roster page cannot say different things about the same row.
async fn warrants_for(neighbours: &[Neighbour]) -> Result<BTreeMap<String, String>, String> {
    let root = sovereign_cli_shared::dirs::sovereign_root();
    let namespaces = commonwealth_rail::namespaces_in(&root).map_err(|e| e.to_string())?;
    let mut rings: Vec<(
        String,
        commonwealth_rail::Roster,
        Vec<commonwealth_rail::AdmittedOp>,
    )> = Vec::new();
    for ns in namespaces {
        // A ring the daemon cannot answer for is SKIPPED and the others are
        // still read: one unreadable journal must not turn every seller's
        // warrant into an error.
        if let Ok((roster, admission)) = sovereign_cli_shared::rail::roster_and_admission(&ns).await
        {
            rings.push((ns, roster, admission.ops));
        }
    }
    Ok(neighbours
        .iter()
        .map(|n| (n.name.clone(), warrant_of(&rings, n.pubkey.as_deref())))
        .collect())
}

/// Resolve ONE seller's warrant, by KEY, across the rings this node holds.
///
/// # The substitution this refuses
///
/// **The join is on the key and never on the name.** A mesh member called
/// Mira and a roster row called Mira are two different assertions, and
/// matching them would let this node answer a question about a REMOTE party
/// out of its own local name table — confidently, plausibly, and wrong the
/// first time two people share a first name. `ring roster show` guards the
/// local version of the same substitution by warning and exiting 1; this is
/// the remote version, and the answer is `warrant unknown`, which is an
/// honest absence (ARCH principle 6).
///
/// A key claimed by more than one ring gets one line per ring. Picking one
/// would be picking an answer out of the air, and the rings are genuinely
/// different local namespaces — the same key can be Mira in one and M. in
/// another, which is the SPKI/SDSI property rather than a conflict.
fn warrant_of(
    rings: &[(
        String,
        commonwealth_rail::Roster,
        Vec<commonwealth_rail::AdmittedOp>,
    )],
    pubkey: Option<&str>,
) -> String {
    let Some(key) = pubkey else {
        return NO_KEY.to_string();
    };
    let found: Vec<String> = rings
        .iter()
        .filter_map(|(ns, roster, ops)| {
            // `person_for` is a KEY lookup. There is deliberately no fallback
            // that looks the member's display name up in `roster.members`.
            let person = roster.person_for(key)?;
            Some(format!(
                "{} [{ns}]",
                commonwealth_rail::trace(roster, ops, person, key)
            ))
        })
        .collect();
    if found.is_empty() {
        return UNCLAIMED.to_string();
    }
    found.join("\n             ")
}

// ─── the two smaller shapes ─────────────────────────────────────────────────

/// `svrn mesh offers --who` — the gossip list. Nothing is dialed, so this
/// answers "who says they publish one", never "who answers".
async fn list_publishers(client: &reqwest::Client, port: u16, json_out: bool) -> i32 {
    let url = format!("http://127.0.0.1:{port}/v1/mesh/offers");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh offers: daemon at {url} not reachable: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        eprint!(
            "{}",
            crate::mesh_skew::render_failure(client, port, "GET /v1/mesh/offers", status, body)
                .await
        );
        return 1;
    }
    if json_out {
        println!("{body}");
        return 0;
    }
    let doc: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mesh offers: response shape mismatch ({e}): {body}");
            return 1;
        }
    };
    let offering = doc["offering"].as_array().cloned().unwrap_or_default();
    if offering.is_empty() {
        println!(
            "No member advertises an offer origin. A holder declares one with\n\
             `[iroh] offer_origin = \"127.0.0.1:8710\"` and RESTARTS its daemon; it shows\n\
             here within one gossip round.\n\n\
             This is gossip only and nothing is dialed — `svrn mesh offers` asks."
        );
        return 0;
    }
    // The sentence below is the same in BOTH branches, deliberately: it is
    // this verb's own claim about itself (it reads gossip and asks nobody),
    // it is true whether the list is empty or not, and it is what the
    // contract journey asserts on — a lane host with no neighbours and one
    // with ten must both prove the verb ran rather than exited 0 quietly.
    println!("Members advertising an offer origin — gossip only, nothing is dialed:");
    for o in &offering {
        println!(
            "  {:<16} {}  {}",
            o["peer"].as_str().unwrap_or("?"),
            o["node_id"].as_str().unwrap_or("?"),
            format!("{}", o["status"].as_str().unwrap_or("?")).to_ascii_lowercase()
        );
    }
    println!();
    println!("See what they have:  svrn mesh offers");
    0
}

/// `svrn mesh offers <peer>` — the loopback URL that reaches ONE member's
/// offer origin, the shape `svrn mesh media <peer>` has.
async fn reach_one(client: &reqwest::Client, port: u16, peer: &str, json_out: bool) -> i32 {
    let url = format!("http://127.0.0.1:{port}/v1/mesh/offers");
    let resp = match client.get(&url).query(&[("peer", peer)]).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh offers: daemon at {url} not reachable: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        eprint!(
            "{}",
            crate::mesh_skew::render_failure(client, port, "GET /v1/mesh/offers", status, body)
                .await
        );
        return 1;
    }
    if json_out {
        println!("{body}");
        return 0;
    }
    let reach: sovereign_mesh::media_reach::MediaReach = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh offers: response shape mismatch ({e}): {body}");
            return 1;
        }
    };
    println!("{}", reach.url);
    println!("  peer  {} ({})", reach.peer, reach.node_id);
    println!("  This is a bridge, not a probe: it accepts whether or not the far end");
    println!("  answers. `svrn mesh offers` asks and prints what came back.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_rail::{AdmittedOp, Introduce, OpId, Person, Roster, Vouch};

    fn key(byte: &str) -> String {
        byte.repeat(32)
    }

    fn intro_op(id: &str, actor: &str, person: &str, subject_key: &str, at: i64) -> AdmittedOp {
        AdmittedOp {
            id: OpId::from_raw(id),
            actor: actor.to_string(),
            person: Person::from("Jonas"),
            corrects: None,
            seq: 1,
            ts_unix: at,
            payload: Some(
                Introduce::new(Person::from(person), subject_key, "sold me the drill")
                    .payload()
                    .expect("canonical"),
            ),
            voided: false,
        }
    }

    /// THE NO-SUBSTITUTION RULE, and it is the load-bearing test in this file.
    ///
    /// A neighbour whose mesh NAME matches a roster row, but whose KEY does
    /// not, must render warrant-unknown. The failing input is a name-based
    /// fallback: it would answer a question about a remote party out of this
    /// node's own local name table, which is exactly the substitution ARCH
    /// principle 6 refuses.
    #[test]
    fn a_sellers_name_matching_a_roster_row_is_not_a_warrant() {
        let mut roster = Roster::new(Default::default());
        roster.bind_key(
            Person::from("Mira"),
            key("aa"),
            Some(Vouch {
                op: OpId::from_raw("ring-1"),
                by: key("bb"),
                at: 1_700_000_000,
            }),
        );
        let ops = vec![intro_op(
            "ring-1",
            &key("bb"),
            "Mira",
            &key("aa"),
            1_700_000_000,
        )];
        let rings = vec![("house".to_string(), roster, ops)];

        // The ring claims key aa as Mira. A DIFFERENT key that happens to
        // belong to a mesh member also called Mira resolves to nothing.
        assert_eq!(warrant_of(&rings, Some(&key("cc"))), UNCLAIMED);
        // And the key the ring does claim resolves, so the test above is not
        // passing because the join never runs at all.
        let traced = warrant_of(&rings, Some(&key("aa")));
        assert!(traced.contains("introduced by"), "{traced}");
        assert!(traced.contains("[house]"), "{traced}");
    }

    /// A member the mesh has no key for is an absence reported, not a blank
    /// and not a guess.
    #[test]
    fn a_member_with_no_gossiped_key_reads_as_unknown_with_its_reason() {
        assert_eq!(warrant_of(&[], None), NO_KEY);
    }

    /// No rings at all still renders a sentence. The failing input is an
    /// empty string, which reads on a terminal as "this row has no warrant
    /// line" rather than "no ring here claims this key".
    #[test]
    fn a_node_holding_no_rings_still_says_why() {
        assert_eq!(warrant_of(&[], Some(&key("aa"))), UNCLAIMED);
    }

    /// Two rings claiming one key is TWO lines, not a pick. They are
    /// different local namespaces and both answers are true.
    #[test]
    fn a_key_two_rings_claim_renders_both() {
        let mk = |person: &str| {
            let mut r = Roster::new(Default::default());
            r.bind_key(Person::from(person), key("aa"), None);
            r
        };
        let rings = vec![
            ("house".to_string(), mk("Mira"), Vec::new()),
            ("dinner".to_string(), mk("M."), Vec::new()),
        ];
        let out = warrant_of(&rings, Some(&key("aa")));
        assert!(out.contains("[house]"), "{out}");
        assert!(out.contains("[dinner]"), "{out}");
        assert_eq!(out.lines().count(), 2, "{out}");
    }

    /// The seller's bytes are shown, never parsed into fields. The failing
    /// input is a renderer that reaches for `item` or `title` — a schema this
    /// repository has no right to.
    #[test]
    fn the_preview_is_the_origins_own_bytes() {
        let body = r#"[{"thing":"drill","terms":"back by Sunday"}]"#;
        assert_eq!(preview(body), vec![body.to_string()]);
        let long = "x".repeat(BODY_PREVIEW_BYTES * 2);
        let out = preview(&long);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), BODY_PREVIEW_BYTES);
        assert!(out[1].contains("cut"));
    }
}
