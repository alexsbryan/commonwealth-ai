// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared argument parsing for `svrn chat` subcommands.
//!
//! Every subcommand takes the same global flags (`--daemon`, `--data-dir`,
//! `--chat-model`, `--embed-model`). This module parses them out of the
//! remaining argv and returns both the resolved config and the leftover
//! positional tokens each subcommand is free to interpret.

use std::path::PathBuf;

use sovereign_core::setup_config::SetupConfig;

use sovereign_cli_shared::urls::DEFAULT_CLIENT_PORT;

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
    /// hard defaults (localhost:9741 + ~/.svrnmesh). Never fails —
    /// a missing config is a fresh-install state, not an error.
    fn default_from_setup() -> Self {
        let (daemon_base, data_dir) = match SetupConfig::load() {
            Ok(cfg) => (
                format!("http://localhost:{}", cfg.daemon.client_port),
                cfg.data.dir,
            ),
            Err(_) => (
                format!("http://localhost:{DEFAULT_CLIENT_PORT}"),
                sovereign_contracts::rebrand::svrnmesh_root(),
            ),
        };
        Self {
            daemon_base,
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
/// Separated from [`parse_globals`] so the parser stays a pure function of
/// argv: a test that read the operator's real `~/.svrnmesh/guest.json` would
/// pass or fail depending on whose machine it ran on.
///
/// Returns true iff the link took effect. The stderr banner is not optional —
/// a guest must always be able to see that their question left their machine,
/// and when the window shuts.
pub fn apply_guest_link(globals: &mut ChatGlobals, link: Option<GuestLink>) -> bool {
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
    eprintln!(
        "Guest link: routing to {} for the next {}m ({}).",
        link.url,
        remaining / 60,
        link.summary.as_deref().unwrap_or("scope not stated")
    );
    globals.daemon_base = link.url;
    globals.bearer = Some(link.token);
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
pub fn parse_globals_for_chat(args: &[String]) -> Result<(ChatGlobals, Vec<String>), String> {
    let (mut globals, rest) = parse_globals(args)?;
    apply_guest_link(&mut globals, guest_link::load_live(guest_link::now_secs()));
    Ok((globals, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_link(url: &str) -> GuestLink {
        GuestLink {
            token: "tok".into(),
            url: url.into(),
            expires_at: guest_link::now_secs() + 3_600,
            summary: Some("models: big-27b".into()),
        }
    }

    #[test]
    fn a_guest_link_redirects_the_daemon_and_carries_a_bearer() {
        let (mut g, _) = parse_globals(&svec(&["ask", "hi"])).unwrap();
        assert!(apply_guest_link(&mut g, Some(a_link("http://box:9741"))));
        assert_eq!(g.daemon_base, "http://box:9741");
        assert_eq!(g.bearer.as_deref(), Some("tok"));
    }

    /// An endpoint the operator typed is the more specific instruction. This
    /// is the arm that keeps `--daemon` from being silently overridden.
    #[test]
    fn an_explicit_daemon_flag_beats_a_stored_guest_link() {
        let (mut g, _) = parse_globals(&svec(&["--daemon", "http://mine:9741", "ask"])).unwrap();
        assert!(!apply_guest_link(&mut g, Some(a_link("http://box:9741"))));
        assert_eq!(g.daemon_base, "http://mine:9741");
        assert!(g.bearer.is_none());
    }

    #[test]
    fn no_link_leaves_the_local_daemon_and_no_bearer() {
        let (mut g, _) = parse_globals(&svec(&["ask"])).unwrap();
        let before = g.daemon_base.clone();
        assert!(!apply_guest_link(&mut g, None));
        assert_eq!(g.daemon_base, before);
        assert!(g.bearer.is_none());
    }

    fn svec(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
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
