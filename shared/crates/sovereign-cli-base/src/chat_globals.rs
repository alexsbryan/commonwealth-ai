// SPDX-License-Identifier: AGPL-3.0-or-later
//! The global flags every chat-shaped verb takes (`--daemon`, `--data-dir`,
//! `--chat-model`, `--embed-model`, `--temperature`, `--max-tokens`). Moved
//! from sovereign-cli-llm's `chat_cmd::config` (pb-cli-llm-bench-move) so
//! bench and svrn's `__probe`, which parses bench's `to_argv`, share one
//! parser. The guest-link overlay stays in `chat_cmd::config`.

use std::path::PathBuf;

use sovereign_contracts::setup_config::SetupConfig;

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
pub fn default_globals_for_voice_eval() -> Result<ChatGlobals, String> {
    ChatGlobals::default_from_setup()
}

impl ChatGlobals {
    /// Seed from `SetupConfig` when it exists; otherwise fall back to
    /// hard defaults (`~/.svrnmesh`). A missing config is a fresh-install
    /// state, not an error; one that exists and does not load is.
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
    fn default_from_setup() -> Result<Self, String> {
        let daemon_base = sovereign_contracts::setup_config::client_daemon_base()?;
        let data_dir = match SetupConfig::load() {
            Ok(cfg) => cfg.data.dir,
            Err(_) => sovereign_contracts::rebrand::svrnmesh_root(),
        };
        Ok(Self {
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
        })
    }
}

/// Parse `args` for the global flags listed in mod.rs's HELP and
/// return `(globals, leftover)`. Leftover tokens keep their order so
/// subcommands can positional-parse them (e.g. `ask "the question"`).
pub fn parse_globals(args: &[String]) -> Result<(ChatGlobals, Vec<String>), String> {
    let mut globals = ChatGlobals::default_from_setup()?;
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

impl ChatGlobals {
    /// The argv [`parse_globals`] turns back into these globals, for a verb
    /// that hands its session config to a child process (`eval run` to
    /// `svrn __probe`). Only what the operator set is written, so the child
    /// resolves every default exactly as the parent did.
    pub fn to_argv(&self) -> Vec<String> {
        let mut argv = Vec::new();
        let mut push = |flag: &str, v: String| {
            argv.push(flag.to_string());
            argv.push(v);
        };
        if self.daemon_explicit {
            push("--daemon", self.daemon_base.clone());
        }
        if self.data_dir_explicit {
            push("--data-dir", self.data_dir.display().to_string());
        }
        if let Some(m) = &self.chat_model {
            push("--chat-model", m.clone());
        }
        if let Some(m) = &self.embed_model {
            push("--embed-model", m.clone());
        }
        if let Some(t) = self.temperature {
            push("--temperature", t.to_string());
        }
        if let Some(n) = self.max_tokens {
            push("--max-tokens", n.to_string());
        }
        argv
    }
}
