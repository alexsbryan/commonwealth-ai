// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared argument parsing for `svrn chat` subcommands.
//!
//! Every subcommand takes the same global flags (`--daemon`, `--data-dir`,
//! `--chat-model`, `--embed-model`). This module parses them out of the
//! remaining argv and returns both the resolved config and the leftover
//! positional tokens each subcommand is free to interpret.

use std::path::PathBuf;

use sovereign_core::setup_config::SetupConfig;

use crate::guest_link::{self, GuestLink};

/// Shared config resolved once per subcommand invocation. Subcommands
/// pass this to `bootstrap::build_runtime` and consult it for output-
/// format decisions (JSON vs text, reasoning visibility, etc.).
#[derive(Debug, Clone)]
pub struct ChatGlobals {
    /// Daemon base URL, `http://host:port` (NO trailing `/v1`). The
    /// `RemoteApiProvider` that talks to `/v1/chat/completions` gets
    /// `{base}/v1` appended internally.
    pub daemon_base: String,
    /// Root of mutable state: `<data_dir>/sovereign.db` is the state
    /// store, `<data_dir>/indexes` is where corpus-engine opens LanceDB
    /// indexes from.
    pub data_dir: PathBuf,
    /// Chat model ID to send in `model:` on every request. `None`
    /// means "auto-resolve from /v1/models" at bootstrap time.
    pub chat_model: Option<String>,
    /// Embedding model ID. Same auto-resolution rule.
    pub embed_model: Option<String>,
    /// True iff `--data-dir` was passed explicitly. Lets bootstrap
    /// decide whether to override the well-known `~/.svrnmesh/indexes`
    /// corpus path with `<data_dir>/indexes`.
    pub data_dir_explicit: bool,
    /// Override `InferenceConfig::temperature` for every chat completion
    /// driven by this session. `None` keeps the runtime default (0.7),
    /// suitable for free-form interactive chat. Set to `Some(0.0)` for
    /// rule-following / deterministic flows — eval, regression
    /// benchmarks, the routing→retrieval→synthesis pipeline where the
    /// goal is to extract facts that downstream tools can consume.
    pub temperature: Option<f32>,
    /// Override `InferenceConfig::max_tokens` for every chat completion
    /// driven by this session. `None` keeps the runtime default. Used
    /// by the eval CLI to sweep the latency/coverage tradeoff (smaller
    /// budget = faster wall, less verbose answer) without touching the
    /// operator's product config. Internal pipeline steps (router
    /// classifier, gap check, planner, etc.) keep their own
    /// hardcoded caps regardless of this override.
    pub max_tokens: Option<usize>,
    /// `Authorization: Bearer` for every outbound call to `daemon_base`.
    ///
    /// `None` for the ordinary case — a loopback caller is admitted by the
    /// daemon before any bearer is read. `Some` only when a guest link is in
    /// effect (`svrn mesh use`), where `daemon_base` points at somebody else's
    /// machine and this is the credential that says the window is still open.
    pub bearer: Option<String>,
    /// True iff a guest link is in effect for this invocation.
    ///
    /// Distinct from `bearer.is_some()`, which is what this used to be read
    /// off. A link no longer sets a bearer — the DAEMON holds the grant and
    /// the turn runs there — so the two facts came apart, and bootstrap needs
    /// this one: under a link the local `SetupConfig`'s model names are the
    /// wrong answer, and `/v1/models` (which now lists the granted ids) is
    /// the right one.
    pub guest_link_active: bool,
    /// The lending node's display URL, when a guest link is in effect.
    ///
    /// Bootstrap needs it to pick a GRANTED model rather than whichever
    /// non-embed id `/v1/models` happens to list first. Without this the
    /// guest borrows a model and then asks their own local slot the
    /// question — which is what happened on the first live 3.3.
    pub guest_lender_url: Option<String>,
    /// True iff `--daemon` was passed explicitly. A guest link must never
    /// override an endpoint the operator named on the command line — an
    /// explicit `--daemon` is the more specific instruction, and silently
    /// redirecting it would be the §18.3 substitution in the surface built to
    /// prevent it.
    pub daemon_explicit: bool,
    /// Standing answering instructions for this session, threaded into
    /// `InferenceConfig::custom_instructions` (the general persona layer —
    /// the outermost system-prompt block). `None` for ordinary chat. A
    /// single-purpose CLI (e.g. `govern ask`) sets this to supply its own
    /// answering discipline without the runtime knowing the domain.
    pub custom_instructions: Option<String>,
}

/// Public default factory for callers (currently `voice_eval`)
/// that don't run the full `parse_globals` argument scan but still
/// need a sensibly-defaulted `ChatGlobals`. Returns the same shape
/// as a no-flag chat invocation: daemon at the configured client
/// port, `~/.svrnmesh` data_dir, no model overrides.
pub fn default_globals_for_voice_eval() -> ChatGlobals {
    ChatGlobals::default_from_setup()
}

impl ChatGlobals {
    /// Seed from `SetupConfig` when it exists; otherwise fall back to
    /// hard defaults (`~/.svrnmesh`). Never fails — a missing config is a
    /// fresh-install state, not an error.
    ///
    /// The daemon base comes from [`client_daemon_base`], NOT from a second
    /// reading of `[daemon] client_port`. It used to be the latter, which
    /// made `svrn chat` blind to `SOVEREIGN_DAEMON_URL` — so pointing a
    /// session at a second daemon moved `svrn enrich` and left the CHAT verb
    /// talking to the operator's local one, and it answered normally while
    /// doing it. A wrong daemon that responds is worse than one that refuses
    /// (§18.3), and two resolvers for one endpoint is the §10.6 shape the
    /// decider exists to collapse.
    ///
    /// Precedence end to end: `--daemon` (parsed after this, and it sets
    /// `daemon_explicit`) > the env knob > `[daemon] client_port` > compiled
    /// default. The flag still wins because an endpoint the operator typed is
    /// the more specific instruction.
    fn default_from_setup() -> Self {
        let daemon_base = sovereign_core::setup_config::client_daemon_base();
        let data_dir = match SetupConfig::load() {
            Ok(cfg) => cfg.data.dir,
            Err(_) => sovereign_contracts::rebrand::svrnmesh_root(),
        };
        Self {
            daemon_base,
            guest_link_active: false,
            guest_lender_url: None,
            data_dir,
            chat_model: None,
            embed_model: None,
            data_dir_explicit: false,
            bearer: None,
            daemon_explicit: false,
            temperature: None,
            max_tokens: None,
            custom_instructions: None,
        }
    }
}

/// Parse `args` for the global flags listed in mod.rs's HELP and
/// return `(globals, leftover)`. Leftover tokens keep their order so
/// subcommands can positional-parse them (e.g. `ask "the question"`).
pub fn parse_globals(args: &[String]) -> Result<(ChatGlobals, Vec<String>), String> {
    let mut globals = ChatGlobals::default_from_setup();
    let mut rest = Vec::with_capacity(args.len());

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--daemon" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--daemon needs a value".to_string())?;
                globals.daemon_base = v.trim_end_matches('/').trim_end_matches("/v1").to_string();
                globals.daemon_explicit = true;
                i += 2;
            }
            "--data-dir" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--data-dir needs a value".to_string())?;
                globals.data_dir = PathBuf::from(v);
                globals.data_dir_explicit = true;
                i += 2;
            }
            "--chat-model" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--chat-model needs a value".to_string())?;
                globals.chat_model = Some(v.clone());
                i += 2;
            }
            "--embed-model" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--embed-model needs a value".to_string())?;
                globals.embed_model = Some(v.clone());
                i += 2;
            }
            "--temperature" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--temperature needs a value".to_string())?;
                let t: f32 = v
                    .parse()
                    .map_err(|_| format!("--temperature: not a float: {v}"))?;
                if !(0.0..=2.0).contains(&t) {
                    return Err(format!("--temperature must be in [0.0, 2.0], got {t}"));
                }
                globals.temperature = Some(t);
                i += 2;
            }
            "--max-tokens" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-tokens needs a value".to_string())?;
                let n: usize = v
                    .parse()
                    .map_err(|_| format!("--max-tokens: not a positive integer: {v}"))?;
                if n == 0 {
                    return Err("--max-tokens must be > 0".to_string());
                }
                globals.max_tokens = Some(n);
                i += 2;
            }
            _ => {
                rest.push(arg.clone());
                i += 1;
            }
        }
    }

    Ok((globals, rest))
}

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
    // (`sovereign_mesh::guest_lender`), which is also why no bearer is set
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
    let base = guest_link::open_route(&link).await?;
    apply_guest_link(&mut globals, Some(link), base);
    Ok((globals, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

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
