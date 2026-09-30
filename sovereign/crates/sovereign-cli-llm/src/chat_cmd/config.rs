// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared argument parsing for `svrn chat` subcommands.
//!
//! Every subcommand takes the same global flags (`--daemon`, `--data-dir`,
//! `--chat-model`, `--embed-model`). This module parses them out of the
//! remaining argv and returns both the resolved config and the leftover
//! positional tokens each subcommand is free to interpret.

use sovereign_cli_shared::guest_link::{self, GuestLink};

pub use sovereign_cli_base::chat_globals::{
    default_globals_for_voice_eval, parse_globals, ChatGlobals,
};

/// Point `globals` at a guest link, if one is in effect and the operator did
/// not name an endpoint themselves.
///
/// `base` is where the link actually resolves to — the lender's URL for a
/// direct link, a loopback tunnel port for a dialled one. It is passed in
/// rather than read off the link because turning a link into an address is
/// `guest_link::open_route`'s job and only its job: a second reader that took
/// `link.url` would send the bearer in plaintext to a mesh that closed its
/// plaintext ingress on purpose.
///
/// Separated from [`parse_globals`] so the parser stays a pure function of
/// argv: a test that read the operator's real `~/.svrnmesh/guest.json` would
/// pass or fail depending on whose machine it ran on.
///
/// Returns true iff the link took effect. The stderr banner is not optional —
/// a guest must always be able to see that their question left their machine,
/// and when the window shuts.
pub fn apply_guest_link(globals: &mut ChatGlobals, link: Option<GuestLink>, base: String) -> bool {
    let Some(link) = link else {
        return false;
    };
    if globals.daemon_explicit {
        eprintln!(
            "(a guest link for {} is stored, but --daemon was given explicitly — using that)",
            link.url
        );
        return false;
    }
    let remaining = link
        .remaining_secs(guest_link::now_secs())
        .unwrap_or_default();
    // The banner names the LENDER, never the loopback bridge port: the guest
    // needs to know whose machine is answering, and `127.0.0.1:41000` would
    // say the opposite of the truth.
    eprintln!(
        "Guest link: routing to {}{} for the next {}m ({}).",
        link.url,
        if link.dial.is_some() {
            " over the mesh tunnel"
        } else {
            ""
        },
        remaining / 60,
        link.summary.as_deref().unwrap_or("scope not stated")
    );
    // The link decides WHICH MODEL, never WHERE THE TURN RUNS.
    //
    // Until 2026-08-28 this set `daemon_base` to the lender and put the grant
    // token on every outbound call. That sent the whole CONVERSATION there —
    // `svrn chat ask` is a surface, the turn runs on a daemon — and
    // `/v1/conversations` is in no `Scope` and is not served on the guest
    // listener at all. Observed: `POST <bridge>/v1/conversations -> 403`
    // (live bar 3.3). A guest's conversation is their own state.
    //
    // So the base stays LOCAL. The guest's own daemon runs the turn and
    // resolves the granted id to the lender itself
    // (`sovereign_serving_host::guest_lender`), which is also why no bearer is set
    // here: the daemon holds the grant, and a token aimed at our own loopback
    // daemon would be meaningless.
    //
    // `base` is still taken, because opening the tunnel is what proves the
    // lender is reachable before we tell the guest their question is going
    // there — a banner promising a live link we never dialled is the failure
    // this whole surface refuses.
    let _ = base;
    globals.guest_link_active = true;
    globals.guest_lender_url = Some(link.url.clone());
    true
}

/// [`parse_globals`] plus the guest-link consult, for the two verbs that
/// actually send completions somewhere: `svrn chat ask` and the interactive
/// `svrn chat` session.
///
/// Deliberately NOT folded into `parse_globals` itself. That parser is shared
/// with local-maintenance verbs (`atlas backfill-ann`, `atlas migrate-all`)
/// which operate on THIS machine's corpora; silently pointing those at a
/// lender's node would be a different command than the one that was typed.
///
/// Async because a link that names an iroh endpoint has to have its tunnel
/// opened before there is an address to point at, and that tunnel must be live
/// for the rest of the process.
pub async fn parse_globals_for_chat(args: &[String]) -> Result<(ChatGlobals, Vec<String>), String> {
    let (mut globals, rest) = parse_globals(args)?;
    let Some(link) = guest_link::load_live(guest_link::now_secs()) else {
        return Ok((globals, rest));
    };
    // An explicit `--daemon` wins, and must do so WITHOUT opening a tunnel:
    // dialing a lender we are then not going to talk to would cost the guest a
    // relay round-trip for nothing, and would log a route that never carried a
    // request.
    if globals.daemon_explicit {
        apply_guest_link(&mut globals, Some(link), String::new());
        return Ok((globals, rest));
    }
    // A stored link that cannot be reached is an ERROR, not a silent fallback
    // to the local daemon: answering a guest's question with a different
    // machine's model and not saying so is the §18.3 substitution this whole
    // surface refuses.
    let base = daemon_guest_route(&globals.daemon_base).await?;
    apply_guest_link(&mut globals, Some(link), base);
    Ok((globals, rest))
}

/// Ask the local daemon where the stored guest link resolves to
/// (`GET /internal/guest/route`). The daemon owns the mesh tunnel (§12 D6),
/// so this verb never opens one itself; no daemon, no link, or a tunnel that
/// will not open each come back as a named error, never a local base.
async fn daemon_guest_route(daemon_base: &str) -> Result<String, String> {
    let url = format!("{daemon_base}/internal/guest/route");
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| {
            format!(
                "a guest link is stored, but the local daemon at {daemon_base} did not answer \
                 ({e}). The daemon opens the route to the lender; start it with \
                 `svrn daemon start`. There is no fallback to answering locally."
            )
        })?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("{url} answered {status} with an unreadable body: {e}"))?;
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{url} answered {status} with a non-JSON body ({e}): {text}"))?;
    if !status.is_success() {
        let error = body["error"].as_str().unwrap_or(&text);
        tracing::info!(%status, error, "guest link: daemon reported no route");
        return Err(format!(
            "could not route the guest link ({status}): {error}"
        ));
    }
    let base = body["base_url"]
        .as_str()
        .ok_or_else(|| format!("{url} answered {status} without a base_url"))?;
    tracing::debug!(base, "guest link: daemon served the route");
    Ok(base.to_string())
}

/// The env prefixes the grounding gate reads its knobs under
/// (`runtime/grounding/config.rs` — `SOVEREIGN_GATE_AUDIT_FORENSICS`,
/// `SOVEREIGN_GATE_BATCH_MIN_CLAIMS`, `SOVEREIGN_GATE_CLAIM_SEARCH`, …;
/// the boot bridge maps the legacy prefix to `SVRNMESH_`, so both spellings
/// count, matching every other reader of the pair).
const GATE_KNOB_PREFIXES: [&str; 2] = ["SOVEREIGN_GATE_", "SVRNMESH_GATE_"];

/// The gate knobs set in `env`, sorted. Pure over an env slice so the
/// detection is testable without touching process state.
pub fn gate_knobs_in<I, K, V>(env: I) -> Vec<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
{
    let mut found: Vec<String> = env
        .into_iter()
        .map(|(k, _)| k.as_ref().to_string())
        .filter(|k| GATE_KNOB_PREFIXES.iter().any(|p| k.starts_with(p)))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// What a caller of `svrn chat ask` / `svrn chat session` needs to hear
/// BEFORE the turn runs, given that the turn runs on the daemon and not in
/// this process (TOPOLOGY §10 phase 6). Two silent failures this closes,
/// both observed while reproducing issue #57:
///
/// 1. The grounding gate reads its knobs with `std::env::var` in the
///    process that runs the turn — the daemon — so exporting
///    `SOVEREIGN_GATE_*` in the shell before `svrn chat ask` changes
///    nothing, and nothing said so.
/// 2. `--data-dir` is parsed for every chat verb, but for a daemon-served
///    turn it only names a local store the forwarded turn never reads. A
///    user passing it to isolate corpora was silently ignored; the flag
///    that actually scopes the turn is `--corpus`.
///
/// Returns the stderr lines (zero, one or two). Pure over the env slice so
/// both triggers have a failing input a test can name (§18.1). Stderr only:
/// `--format json` stdout stays a clean payload.
pub fn daemon_served_turn_notes<I, K, V>(globals: &ChatGlobals, env: I) -> Vec<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
{
    let mut notes = Vec::new();
    let knobs = gate_knobs_in(env);
    if !knobs.is_empty() {
        notes.push(format!(
            "note: {} set in this shell, but the turn runs on the daemon at {} — gate knobs \
             are read from the DAEMON's environment, so these have no effect here. Export them \
             where the daemon is launched (`svrn daemon start` from a shell that exports them, \
             or the service manager's environment) and restart it.",
            knobs.join(", "),
            globals.daemon_base
        ));
    }
    if globals.data_dir_explicit {
        notes.push(format!(
            "note: --data-dir ({}) does not scope a daemon-served turn — the daemon at {} reads \
             its own data dir, and this flag only names a local store the forwarded turn never \
             reads. To restrict retrieval to a corpus, pass --corpus <id> (repeatable).",
            globals.data_dir.display(),
            globals.daemon_base
        ));
    }
    notes
}

/// [`daemon_served_turn_notes`] over the real process environment, printed
/// to stderr. The one call site per turn-sending verb.
pub fn print_daemon_served_turn_notes(globals: &ChatGlobals) {
    for line in daemon_served_turn_notes(globals, std::env::vars()) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No daemon is a named error, never a base: a fallback that answered
    /// locally would come back `Ok` and fail this.
    #[tokio::test]
    async fn no_local_daemon_is_a_named_absence_not_a_route() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let error = daemon_guest_route(&format!("http://127.0.0.1:{port}"))
            .await
            .unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        assert!(error.contains("no fallback"), "{error}");
    }

    /// `to_argv` is `parse_globals`' inverse: a child handed it resolves the
    /// same globals, and a flag the operator did not pass stays unwritten.
    #[test]
    fn to_argv_parses_back_to_the_same_globals() {
        let (g, _) = parse_globals(&svec(&[
            "--daemon",
            "http://box:9999/v1",
            "--data-dir",
            "/tmp/d",
            "--chat-model",
            "c",
            "--embed-model",
            "e",
            "--temperature",
            "0.3",
            "--max-tokens",
            "64",
            "run",
        ]))
        .unwrap();
        let (back, rest) = parse_globals(&g.to_argv()).unwrap();
        assert!(rest.is_empty(), "{rest:?}");
        assert_eq!(back.daemon_base, g.daemon_base);
        assert!(back.daemon_explicit && back.data_dir_explicit);
        assert_eq!(back.data_dir, g.data_dir);
        assert_eq!(back.chat_model.as_deref(), Some("c"));
        assert_eq!(back.embed_model.as_deref(), Some("e"));
        assert_eq!(back.temperature, Some(0.3));
        assert_eq!(back.max_tokens, Some(64));

        let (bare, _) = parse_globals(&svec(&["run"])).unwrap();
        assert!(bare.to_argv().is_empty());
    }

    fn a_link(url: &str) -> GuestLink {
        GuestLink {
            token: "tok".into(),
            url: url.into(),
            dial: None,
            expires_at: guest_link::now_secs() + 3_600,
            summary: Some("models: big-27b".into()),
        }
    }

    /// THE 3.3 regression. A guest link must NOT move where the turn runs.
    ///
    /// It used to set `daemon_base` to the lender and put the grant on every
    /// call, which sent the CONVERSATION there — `svrn chat ask` is a
    /// surface, the turn runs on a daemon — and `/v1/conversations` is in no
    /// `Scope` and is not served on the guest listener. Live bar 3.3
    /// observed `POST <bridge>/v1/conversations -> 403` before this changed.
    ///
    /// Watched failing: restore either assignment and this goes red.
    #[test]
    fn a_guest_link_does_not_move_where_the_turn_runs() {
        let (mut g, _) = parse_globals(&svec(&["ask", "hi"])).unwrap();
        let before = g.daemon_base.clone();
        assert!(apply_guest_link(
            &mut g,
            Some(a_link("http://box:9741")),
            "http://box:9741".into()
        ));
        assert_eq!(
            g.daemon_base, before,
            "the turn runs on the guest's OWN daemon; only the completion crosses"
        );
        assert!(
            g.bearer.is_none(),
            "the DAEMON holds the grant — a token aimed at our own loopback \
             daemon is meaningless, and setting it is what used to send the \
             conversation to the lender"
        );
        assert!(g.guest_link_active, "but bootstrap must still know");
    }

    /// A dialled link still opens the tunnel before the banner promises the
    /// lender is reachable — the address the link names is closed on an
    /// encrypted mesh, so a link we never dialled is a promise we cannot keep.
    /// The tunnel's base still must not become the turn's daemon.
    #[test]
    fn a_dialled_link_is_still_dialled_but_does_not_capture_the_turn() {
        let (mut g, _) = parse_globals(&svec(&["ask", "hi"])).unwrap();
        let before = g.daemon_base.clone();
        let mut link = a_link("http://box:9741");
        link.dial = Some("beef@https://relay.example".into());
        assert!(apply_guest_link(
            &mut g,
            Some(link),
            "http://127.0.0.1:41007".into()
        ));
        assert_eq!(g.daemon_base, before);
        assert!(g.bearer.is_none());
        assert!(g.guest_link_active);
    }

    /// An endpoint the operator typed is the more specific instruction. This
    /// is the arm that keeps `--daemon` from being silently overridden.
    #[test]
    fn an_explicit_daemon_flag_beats_a_stored_guest_link() {
        let (mut g, _) = parse_globals(&svec(&["--daemon", "http://mine:9741", "ask"])).unwrap();
        assert!(!apply_guest_link(
            &mut g,
            Some(a_link("http://box:9741")),
            "http://box:9741".into()
        ));
        assert_eq!(g.daemon_base, "http://mine:9741");
        assert!(g.bearer.is_none());
        assert!(
            !g.guest_link_active,
            "a refused link must not put bootstrap into guest model-resolution"
        );
    }

    #[test]
    fn no_link_leaves_the_local_daemon_and_no_bearer() {
        let (mut g, _) = parse_globals(&svec(&["ask"])).unwrap();
        let before = g.daemon_base.clone();
        assert!(!apply_guest_link(&mut g, None, String::new()));
        assert_eq!(g.daemon_base, before);
        assert!(g.bearer.is_none());
    }

    fn svec(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // These mutate PROCESS-global env, and after the sweep `parse_globals`
    // READS it — so the same lock/restore discipline as
    // `setup_config`'s own knob tests (§18.1: a gate that passes only under
    // nextest's process-per-test and flakes under `--engine cargo` is not a
    // gate). No existing test in this module asserts an absolute
    // `daemon_base`; every one either passes `--daemon` or compares to a
    // value it captured itself, so a guarded window cannot flip them.
    static DAEMON_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct DaemonEnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prior: Vec<(&'static str, Option<String>)>,
    }

    impl DaemonEnvGuard {
        fn set(pairs: &[(&'static str, &str)]) -> Self {
            let lock = DAEMON_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            const KEYS: [&str; 2] = ["SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL"];
            let prior = KEYS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in KEYS {
                std::env::remove_var(k);
            }
            for (k, v) in pairs {
                std::env::set_var(k, v);
            }
            Self { _lock: lock, prior }
        }
    }

    impl Drop for DaemonEnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.prior {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// RED on the tree before this sweep. `default_from_setup` built the base
    /// from `cfg.daemon.client_port` and could not see the knob, so `svrn
    /// chat` — the verb a second session pointed at a rented daemon actually
    /// drives — kept talking to the OPERATOR's local daemon while
    /// `SOVEREIGN_DAEMON_URL` said otherwise, and answered successfully while
    /// doing it. That is the §18.3 silent substitution, and it is the same
    /// one `client_daemon_base` was minted to close for `svrn enrich`.
    #[test]
    fn chat_globals_honour_the_daemon_knob() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://a-rented-pod:9841")]);
        let (g, _) = parse_globals(&svec(&["ask", "hi"])).unwrap();
        assert_eq!(
            g.daemon_base, "http://a-rented-pod:9841",
            "chat must resolve through the ONE decider, not re-read client_port"
        );
        assert!(
            !g.daemon_explicit,
            "the env is a default, not an operator-typed endpoint — a guest \
             link may still override it"
        );
    }

    /// The precedence the sweep must not disturb: an endpoint the operator
    /// TYPED is more specific than one they exported. `--daemon` wins, and it
    /// still marks itself explicit so a stored guest link cannot displace it.
    #[test]
    fn an_explicit_daemon_flag_beats_the_env_knob() {
        let _g = DaemonEnvGuard::set(&[("SOVEREIGN_DAEMON_URL", "http://a-rented-pod:9841")]);
        let (g, _) = parse_globals(&svec(&["--daemon", "http://mine:9741", "ask"])).unwrap();
        assert_eq!(g.daemon_base, "http://mine:9741");
        assert!(g.daemon_explicit);
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The failing input: a gate knob exported in the CLI's shell. Before
    /// this the turn ran on the daemon with the knob unset and nothing said
    /// so. One line, naming every knob, the daemon it runs on, and the remedy.
    #[test]
    fn gate_knobs_in_the_shell_produce_one_note_naming_them_and_the_daemon() {
        let (g, _) = parse_globals(&svec(&["--daemon", "http://box:9741", "ask"])).unwrap();
        let notes = daemon_served_turn_notes(
            &g,
            env(&[
                ("SOVEREIGN_GATE_BATCH_MIN_CLAIMS", "6"),
                ("PATH", "/usr/bin"),
                ("SVRNMESH_GATE_AUDIT_FORENSICS", "1"),
                ("SOVEREIGN_DAEMON_URL", "http://elsewhere:9741"),
            ]),
        );
        assert_eq!(notes.len(), 1, "one line, not one per knob: {notes:?}");
        let line = &notes[0];
        assert!(
            line.contains("SOVEREIGN_GATE_BATCH_MIN_CLAIMS, SVRNMESH_GATE_AUDIT_FORENSICS"),
            "names every gate knob, sorted: {line}"
        );
        assert!(
            !line.contains("SOVEREIGN_DAEMON_URL") && !line.contains("PATH"),
            "only gate knobs, not every SOVEREIGN_ var: {line}"
        );
        assert!(line.contains("http://box:9741"), "names the daemon: {line}");
        assert!(
            line.contains("svrn daemon start"),
            "carries the remedy: {line}"
        );
    }

    #[test]
    fn no_gate_knobs_and_no_data_dir_is_silent() {
        let (g, _) = parse_globals(&svec(&["ask"])).unwrap();
        assert!(daemon_served_turn_notes(&g, env(&[("PATH", "/usr/bin")])).is_empty());
    }

    /// `--data-dir` on a daemon-served verb: the flag used to be accepted
    /// and ignored. The note says what it does NOT do and names the flag
    /// that does.
    #[test]
    fn an_explicit_data_dir_produces_a_note_pointing_at_corpus() {
        let (g, _) = parse_globals(&svec(&["--data-dir", "/tmp/iso", "ask"])).unwrap();
        let notes = daemon_served_turn_notes(&g, env(&[]));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("--data-dir (/tmp/iso)"), "{}", notes[0]);
        assert!(notes[0].contains("--corpus <id>"), "{}", notes[0]);
    }

    #[test]
    fn a_default_data_dir_is_not_flagged() {
        let (g, _) = parse_globals(&svec(&["ask"])).unwrap();
        assert!(!g.data_dir_explicit);
        assert!(daemon_served_turn_notes(&g, env(&[])).is_empty());
    }

    #[test]
    fn parse_globals_pulls_flags_and_preserves_positional() {
        let (g, rest) = parse_globals(&svec(&[
            "--daemon",
            "http://box:9999",
            "ask",
            "--chat-model",
            "qwen3-8b",
            "hello world",
        ]))
        .unwrap();
        assert_eq!(g.daemon_base, "http://box:9999");
        assert_eq!(g.chat_model.as_deref(), Some("qwen3-8b"));
        assert_eq!(rest, vec!["ask", "hello world"]);
    }

    #[test]
    fn parse_globals_strips_v1_suffix_from_daemon() {
        let (g, _) = parse_globals(&svec(&["--daemon", "http://localhost:9741/v1"])).unwrap();
        assert_eq!(g.daemon_base, "http://localhost:9741");
    }

    #[test]
    fn parse_globals_errors_on_missing_value() {
        let err = parse_globals(&svec(&["--daemon"])).unwrap_err();
        assert!(err.contains("--daemon"));
    }
}
