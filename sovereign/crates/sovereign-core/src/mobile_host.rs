// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared core for **opt-in mobile access** — serving the phone-facing
//! `sovereign-server` API (stateful `/v1/conversations` + `/v1/corpora` + WS
//! streaming) without loading a second copy of the models.
//!
//! ## The one idea
//!
//! The expensive chat/primary (and embed) models are already resident in the
//! local **daemon** (`sovereign daemon`, bound on `daemon.client_port`, default
//! `:9741`, serving OpenAI-style `/v1/chat/completions` + `/v1/embeddings`).
//! Rather than spin up a second model load, the mobile host runs
//! `sovereign-server` configured with a single **`[[inference.backends]]`
//! `type = "remote"`** entry pointed at that daemon. `sovereign-server`'s
//! bootstrap skips the embedded `EmbeddedLlamaCpp::load_full` path entirely
//! when `backends` is non-empty (`sovereign-server/src/main.rs:108`), so the
//! host loads **zero GGUF weights** — it's a thin stateful-conversation +
//! retrieval layer that forwards every completion and every query-embedding to
//! the resident daemon. Corpus indexes are opened straight off disk
//! (`~/.sovereign/indexes`), which needs no model.
//!
//! ## Two front-ends, one core
//!
//! Both the CLI (`sovereign mobile serve`) and the desktop app's "Mobile
//! access" toggle call into here to (1) load-or-create the operator's
//! mobile-host settings + a persisted bearer token, (2) generate the
//! `sovereign-server` config TOML from the already-present [`SetupConfig`], and
//! (3) locate the `sovereign-server` sibling binary. Process lifecycle differs
//! per surface (CLI = foreground / systemd; desktop = its `Supervisor`), so
//! this module deliberately stops at "produce the config + find the binary".

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::setup_config::SetupConfig;

/// Persisted mobile-host settings, at `~/.sovereign/mobile-host.toml` (next to
/// `config.toml`). Holds the operator's choices (bind address, tenant, corpus
/// allow-list) and the auto-generated bearer token the phone authenticates
/// with. The token is the only secret; it is NOT in the generated
/// server-config (that's regenerated each launch), so this file is the single
/// source of truth for pairing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileHostConfig {
    /// Address the host binds. Default `0.0.0.0:8080` — reachable over the
    /// tailnet (the phone dials the node's tailnet IP / MagicDNS name on this
    /// port). Binding `0.0.0.0` rather than the tailnet IP keeps it simple;
    /// fail-closed-off-tailnet is the client's job, and a tailnet-only ACL is
    /// the operator's.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Tenant id the issued token maps to — the phone authenticates as this.
    /// One tenant ("you") is the common case; conversations are scoped
    /// `tenant:conversation_id` server-side, so the same token across devices
    /// shares history.
    #[serde(default = "default_tenant")]
    pub tenant: String,
    /// Bearer token the phone presents (`Authorization: Bearer <token>`).
    /// Auto-generated on first run; stable across restarts so a paired phone
    /// keeps working.
    pub token: String,
    /// Corpus allow-list (empty = every installed corpus). Scopes both
    /// retrieval and the `/v1/corpora` listing the phone sees.
    #[serde(default)]
    pub corpora: Vec<String>,
    /// Dial-by-key access (iroh): the phone reaches this host through
    /// a relay-assisted QUIC tunnel with **no VPN installed**. Default
    /// on — it is additive (the tailnet path is unaffected) and fails
    /// soft (a bind error logs and disables itself). See
    /// `sovereign/docs/specs/TRANSPORT_MIGRATION.md` Track M.
    #[serde(default = "default_iroh_enabled")]
    pub iroh_enabled: bool,
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_tenant() -> String {
    "me".to_string()
}

fn default_iroh_enabled() -> bool {
    true
}

/// What a user needs to pair a phone: where to dial, who they are, and the
/// token. The address here is the configured `bind`; resolving the prettier
/// tailnet MagicDNS name is the front-end's job (the CLI shells out to
/// `tailscale`, the desktop can show both).
#[derive(Debug, Clone)]
pub struct PairingInfo {
    pub bind: String,
    pub tenant: String,
    pub token: String,
}

impl MobileHostConfig {
    /// `~/.sovereign/mobile-host.toml` — co-located with `config.toml` so
    /// operators have one user directory to remember.
    pub fn default_path() -> PathBuf {
        SetupConfig::default_path()
            .parent()
            .map(|p| p.join("mobile-host.toml"))
            .unwrap_or_else(|| PathBuf::from("mobile-host.toml"))
    }

    /// Load the settings, creating them (with a freshly-generated token) on
    /// first use. Idempotent: after the first call the file exists and the
    /// token is stable.
    pub fn load_or_create() -> Result<Self, String> {
        let path = Self::default_path();
        if path.exists() {
            let s = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            toml::from_str(&s).map_err(|e| format!("parse {}: {e}", path.display()))
        } else {
            let cfg = Self {
                bind: default_bind(),
                tenant: default_tenant(),
                token: generate_token(),
                corpora: Vec::new(),
                iroh_enabled: default_iroh_enabled(),
            };
            cfg.save()?;
            Ok(cfg)
        }
    }

    /// Write to `default_path()`, creating `~/.sovereign` if needed.
    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::default_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let toml = toml::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, toml).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path)
    }

    pub fn pairing_info(&self) -> PairingInfo {
        PairingInfo {
            bind: self.bind.clone(),
            tenant: self.tenant.clone(),
            token: self.token.clone(),
        }
    }
}

/// A random bearer token. `sk-mobile-<uuid-simple>` — recognizable as a
/// Sovereign mobile token, opaque, and collision-free.
pub fn generate_token() -> String {
    format!("sk-mobile-{}", uuid::Uuid::new_v4().simple())
}

/// Turn a bind address into one the phone can actually dial. A wildcard host
/// (`0.0.0.0` / `::`) is replaced with this node's Tailscale IPv4 (`tailscale
/// ip -4`) when available, since the phone reaches the host over the tailnet;
/// an explicit host is returned unchanged. Used by both the CLI pairing card
/// and the desktop Mobile-access panel.
pub fn dialable_address(bind: &str) -> String {
    let port = bind
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let host = bind.rsplit_once(':').map(|(h, _)| h).unwrap_or(bind);
    if host == "0.0.0.0" || host == "::" || host.is_empty() {
        if let Some(ip) = tailscale_ipv4() {
            return format!("{ip}:{port}");
        }
        return format!("<this-node-tailnet-ip>:{port}");
    }
    bind.to_string()
}

/// First IPv4 from `tailscale ip -4`, if the CLI is present and logged in.
fn tailscale_ipv4() -> Option<String> {
    let out = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Locate the `sovereign-server` binary to run. Order: `SOVEREIGN_SERVER_PATH`
/// env override, then a sibling of the current executable (the install layout
/// puts every `sovereign-*` binary side-by-side, and the dev `target/debug`
/// layout does too). Returns `None` if neither hits, so callers can surface a
/// clear "build/install sovereign-server" error rather than a spawn failure.
pub fn resolve_server_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOVEREIGN_SERVER_PATH") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?;
    for name in ["sovereign-server", "sovereign-server.exe"] {
        let candidate = parent.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

// ── Generated sovereign-server config ──────────────────────────────────────
//
// We emit the TOML rather than depend on sovereign-server's `ServerConfig`
// (which is `Deserialize`-only and lives in the binary crate). These mirror
// the shape sovereign-server reads; scalar fields precede table/array fields
// so `toml::to_string` emits a valid document.

#[derive(Serialize)]
struct GenServerConfig {
    server: GenServer,
    auth: GenAuth,
    inference: GenInference,
    store: GenStore,
    retrieval: GenRetrieval,
    knowledge_view: GenKnowledgeView,
    iroh: GenIroh,
}

#[derive(Serialize)]
struct GenServer {
    bind: String,
    max_concurrent_turns: usize,
    retry_after_secs: u64,
}

#[derive(Serialize)]
struct GenAuth {
    mode: String,
    keys: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct GenInference {
    // `model` is a required field in sovereign-server's parser, but it is NOT
    // loaded when `backends` is present — it serves only as the source of the
    // embed-model-mismatch label. We point it at the real fast/embed paths so
    // that label is correct, and so removing `backends` degrades gracefully to
    // a local load instead of a missing-file crash.
    model: String,
    embed_model: String,
    context_size: u32,
    backends: Vec<GenBackend>,
}

#[derive(Serialize)]
struct GenBackend {
    name: String,
    #[serde(rename = "type")]
    backend_type: String,
    priority: u32,
    endpoint: String,
    model_id: String,
    context_size: u32,
}

#[derive(Serialize)]
struct GenStore {
    path: String,
}

#[derive(Serialize)]
struct GenRetrieval {
    corpora: Vec<String>,
}

#[derive(Serialize)]
struct GenKnowledgeView {
    enabled: bool,
}

#[derive(Serialize)]
struct GenIroh {
    enabled: bool,
}

/// Generate the `sovereign-server` config TOML that delegates ALL inference
/// (chat + embeddings) to the local daemon, loading no models of its own.
///
/// `setup` supplies the daemon port + the (metadata-only) model paths +
/// context size; `mh` supplies the bind/auth/corpora. The single remote
/// backend points at `http://127.0.0.1:<client_port>/v1`. Fast-vs-primary slot
/// selection happens daemon-side via the OICP `latency_class` the Runtime's
/// `preferred_speed` maps to, so no per-request `model_id` is needed.
pub fn generate_server_toml(setup: &SetupConfig, mh: &MobileHostConfig) -> Result<String, String> {
    let ctx = setup.models.effective_context_size();
    let daemon_endpoint = format!("http://127.0.0.1:{}/v1", setup.daemon.client_port);
    let store_path = setup.data.dir.join("mobile-host.db");

    let mut keys = std::collections::BTreeMap::new();
    keys.insert(mh.token.clone(), mh.tenant.clone());

    let cfg = GenServerConfig {
        server: GenServer {
            bind: mh.bind.clone(),
            max_concurrent_turns: 4,
            retry_after_secs: 2,
        },
        auth: GenAuth {
            mode: "api_key".to_string(),
            keys,
        },
        inference: GenInference {
            model: setup.models.fast_path().display().to_string(),
            embed_model: setup.models.embed.display().to_string(),
            context_size: ctx,
            backends: vec![GenBackend {
                name: "daemon".to_string(),
                backend_type: "remote".to_string(),
                priority: 1,
                endpoint: daemon_endpoint,
                model_id: "default".to_string(),
                context_size: ctx,
            }],
        },
        store: GenStore {
            path: store_path.display().to_string(),
        },
        retrieval: GenRetrieval {
            corpora: mh.corpora.clone(),
        },
        // Off for the mobile host: the conversational-corpus background ingest
        // is heavy and the phone doesn't need it for chat + retrieval. (Flip on
        // later to exercise MOBILE.md §7 conversation-corpus privacy.)
        knowledge_view: GenKnowledgeView { enabled: false },
        // Dial-by-key access for phones with no VPN. The server's
        // `node_key` lands beside the store DB (same data dir); the
        // pairing string surfaces at `GET /status` → `iroh.dial`.
        iroh: GenIroh {
            enabled: mh.iroh_enabled,
        },
    };

    let header = "# Generated by `sovereign mobile` / desktop Mobile-access toggle.\n\
                  # Do NOT edit — regenerated on every launch from ~/.sovereign/config.toml\n\
                  # + ~/.sovereign/mobile-host.toml. Inference rides on the local daemon\n\
                  # (remote backend); this host loads no models of its own.\n\n";
    let body = toml::to_string_pretty(&cfg).map_err(|e| format!("serialize server config: {e}"))?;
    Ok(format!("{header}{body}"))
}

/// Generate the server config and write it to `~/.sovereign/mobile-host-server.toml`,
/// returning the path to hand to `sovereign-server --config`.
pub fn write_server_config(setup: &SetupConfig, mh: &MobileHostConfig) -> Result<PathBuf, String> {
    let toml = generate_server_toml(setup, mh)?;
    let path = SetupConfig::default_path()
        .parent()
        .map(|p| p.join("mobile-host-server.toml"))
        .unwrap_or_else(|| PathBuf::from("mobile-host-server.toml"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, toml).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_config::{DaemonSection, DataSection, ModelsSection};

    fn setup_fixture() -> SetupConfig {
        SetupConfig {
            models: ModelsSection {
                primary: PathBuf::from("/m/primary.gguf"),
                fast: Some(PathBuf::from("/m/fast.gguf")),
                embed: PathBuf::from("/m/qwen-embedding-0.6b.gguf"),
                code: None,
                context_size: Some(16384),
                extra: Default::default(),
                max_extras_memory_gb: None,
                primary_pool: None,
            },
            daemon: DaemonSection::default(),
            data: DataSection {
                dir: PathBuf::from("/home/u/.sovereign"),
            },
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
        }
    }

    fn mh_fixture() -> MobileHostConfig {
        MobileHostConfig {
            bind: "0.0.0.0:8080".to_string(),
            tenant: "alex".to_string(),
            token: "sk-mobile-test".to_string(),
            corpora: vec!["sep".to_string()],
            iroh_enabled: true,
        }
    }

    #[test]
    fn token_is_prefixed_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert!(a.starts_with("sk-mobile-"));
        assert_ne!(a, b);
    }

    #[test]
    fn generated_config_is_remote_only_and_parses_as_toml() {
        let toml_str = generate_server_toml(&setup_fixture(), &mh_fixture()).unwrap();
        // Must be a valid TOML document.
        let value: toml::Value = toml::from_str(&toml_str).unwrap();

        // A single remote backend pointed at the daemon — the load-bearing
        // invariant: no embedded backend means no second model load.
        let backends = value["inference"]["backends"].as_array().unwrap();
        assert_eq!(backends.len(), 1);
        assert_eq!(backends[0]["type"].as_str(), Some("remote"));
        assert_eq!(
            backends[0]["endpoint"].as_str(),
            Some("http://127.0.0.1:9741/v1")
        );

        // Auth wires the token → tenant the phone presents.
        assert_eq!(value["auth"]["mode"].as_str(), Some("api_key"));
        assert_eq!(
            value["auth"]["keys"]["sk-mobile-test"].as_str(),
            Some("alex")
        );

        // Retrieval allow-list + bind carried through.
        assert_eq!(value["server"]["bind"].as_str(), Some("0.0.0.0:8080"));
        let corpora = value["retrieval"]["corpora"].as_array().unwrap();
        assert_eq!(corpora.len(), 1);
        assert_eq!(corpora[0].as_str(), Some("sep"));

        // Dial-by-key access rides the config knob through.
        assert_eq!(value["iroh"]["enabled"].as_bool(), Some(true));
    }

    #[test]
    fn iroh_knob_off_is_emitted_off() {
        let mut mh = mh_fixture();
        mh.iroh_enabled = false;
        let toml_str = generate_server_toml(&setup_fixture(), &mh).unwrap();
        let value: toml::Value = toml::from_str(&toml_str).unwrap();
        assert_eq!(value["iroh"]["enabled"].as_bool(), Some(false));
    }

    #[test]
    fn pre_iroh_mobile_host_toml_parses_with_default_on() {
        // A mobile-host.toml written before the knob existed must load
        // with iroh_enabled = true (the serde default).
        let old = r#"
            bind = "0.0.0.0:8080"
            tenant = "alex"
            token = "sk-mobile-old"
        "#;
        let cfg: MobileHostConfig = toml::from_str(old).unwrap();
        assert!(cfg.iroh_enabled);
    }
}
