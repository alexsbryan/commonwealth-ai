// SPDX-License-Identifier: AGPL-3.0-or-later
//! Desktop configuration — `DesktopConfig`, its defaults, and load/save.
//! Extracted from `state.rs` in the §3.3 decomposition. Pure data + IO
//! (no runtime/inference types), so it unit-tests without a model.

use serde::{Deserialize, Serialize};
use sovereign_core::model_family::ModelFamily;
use std::path::PathBuf;

// ─── Desktop Config ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    // ── NOTE: model-slot *paths* deliberately do NOT live here. ──
    // `model_path` / `primary_model_path` / `embed_model_path` /
    // `code_model_path` were moved to `SetupConfig`
    // (`~/.svrnmesh/config.toml`, the daemon's config), making it the
    // single on-disk source of truth for what each slot loads. The
    // desktop reads/writes them via the `get_setup_model_slots` /
    // `set_setup_model_slots` Tauri commands and the `ResolvedModelSlots`
    // carrier. Keeping paths in one file is what stops the Settings panel
    // and the daemon from ever disagreeing. Old `desktop.toml` files that
    // still carry these keys are migrated once in `DesktopConfig::load`
    // (serde ignores the now-unknown keys thereafter).
    //
    /// Model family of the code slot — drives tokenizer / chat
    /// template quirks. Typically `Qwen35` for Qwen Coder lineage,
    /// `Unknown` for BYOM coders.
    ///
    /// ASYMMETRY (intentional): the code/embed slot *paths* live in
    /// `SetupConfig`, but their *family* hints (`code_family` /
    /// `embed_family`) stay here — they are desktop-side load-time hints
    /// the daemon auto-detects from the GGUF, so they don't participate
    /// in the path single-source-of-truth. Do NOT "consolidate" them into
    /// `ModelsSection`: `sovereign-contracts` can't reference
    /// `sovereign_core::model_family::ModelFamily`.
    #[serde(default)]
    pub code_family: ModelFamily,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_skills_dir")]
    pub skills_dir: PathBuf,
    #[serde(default)]
    pub active_skills: Vec<String>,
    #[serde(default = "default_enabled_tools")]
    pub enabled_tools: Vec<String>,
    // NOTE: `context_size` was removed from this struct — its canonical
    // home is `~/.svrnmesh/config.toml`'s `[models].context_size`
    // (edited via `set_setup_context_size`). Existing `desktop.toml`
    // files that still carry it are migrated once in `DesktopConfig::load`
    // and the now-unknown key is thereafter ignored by serde.
    #[serde(default)]
    pub search_backend: SearchBackendConfig,
    #[serde(default)]
    pub setup_complete: bool,
    #[serde(default)]
    pub selected_tier: Option<String>,
    /// Opt-in for the Recipe Author workspace (M2/M3). When `false`
    /// (the default for new and existing configs), the workspace
    /// switcher is hidden and `recipe_author_*` Tauri commands are
    /// only callable via direct invocation. When `true`, the
    /// ConversationList sidebar exposes a "Recipe Author →" entry
    /// that swaps `App.svelte`'s view to the workspace.
    ///
    /// Surfaced both in the SetupWizard (advanced section) and in
    /// Settings, so a user who skipped it during setup can flip it on
    /// later without restarting through the wizard.
    #[serde(default)]
    pub enable_recipe_authoring: bool,
    /// Opt-in for **Mobile access** — serving the phone-facing
    /// `sovereign-server` API so the svrnmesh mobile app can pair with this
    /// node over the tailnet. When `true`, the desktop supervises a
    /// `sovereign-server` child (via [`crate::supervisor::Supervisor`]) that
    /// delegates all inference to the local daemon — it loads no models of
    /// its own. Off by default; flipped from Settings → Mobile access.
    #[serde(default)]
    pub mobile_access_enabled: bool,
    /// When `true`, the `knowledge_lookup` tool auto-escalates to
    /// web search when the local envelope returns thin or empty
    /// results. The escalation is internal to the tool — the
    /// model sees one unified Evidence envelope with
    /// `source_kind: web` rows alongside any corpus / memory /
    /// note results. The INFORMATION REQUEST card path stays
    /// available regardless — this setting only controls whether
    /// the tool can ALSO go to the web without asking the user.
    ///
    /// Default `false`: users explicitly approve every web call
    /// via the card. Set `true` for hands-off operation when the
    /// user has explicitly chosen to delegate that judgement.
    #[serde(default)]
    pub auto_escalate_to_web: bool,

    // ── Advanced Tuning ─────────────────────────────────────────
    /// Generation temperature (0.0–1.0). Higher = more creative, lower = more focused.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Maximum tokens to generate per response.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Maximum tokens allowed inside a `<think>` block before it is
    /// force-closed, preventing the model from spiralling indefinitely.
    #[serde(default = "default_think_budget")]
    pub think_budget: u32,
    /// Top-k sampling. `None` defers to the model family default.
    /// Bundled with `temperature` in the Creativity preset selector.
    #[serde(default)]
    pub top_k: Option<u32>,
    /// Epistemic humility mode. When true, the runtime audits each
    /// answer for thin evidence and surfaces an `InformationRequest`
    /// card asking the user to paste a source. Default **on**; see
    /// `sovereign_core::types::InferenceConfig::auto_collaborate` for
    /// the full story. Named `#[serde(default = …)]` so existing
    /// saved configs without the field also upgrade to on.
    #[serde(default = "default_auto_collaborate")]
    pub auto_collaborate: bool,

    /// "Naked mode" — run the loaded model raw, with NONE of the
    /// svrnmesh affordances (no retrieval, router, grounding gate,
    /// tools, atlas, or gap-check). Chat history → model → reply, with
    /// only a minimal assistant preamble + `custom_instructions`. For
    /// A/B-ing a model's raw behaviour against the situated agent, or
    /// running a model the affordances don't suit. Default off; routes
    /// chat through `Runtime::handle_message_stream_naked`.
    #[serde(default = "default_naked_mode")]
    pub naked_mode: bool,

    /// Shared-model cluster role: `consumer` (use a mesh-hosted shared model as
    /// primary), `anchor` (lend memory to hold it), or `host` (own the loaded
    /// instance). Default `consumer`. Mirrored to `[shared_model] role`.
    #[serde(default)]
    pub shared_model_role: sovereign_core::setup_config::SharedModelRole,
    /// The shared model id to use/host (as advertised in the mesh). `None` = not
    /// participating. Mirrored to `[shared_model] model_id`.
    #[serde(default)]
    pub shared_model_id: Option<String>,

    /// User-authored "custom instructions" / persona — global standing
    /// guidance appended as the outermost layer of every system prompt
    /// (see `sovereign_core::types::InferenceConfig::custom_instructions`).
    /// Append-only: it never replaces the situated prompt. Empty / `None`
    /// is a no-op (byte-identical prompt). Editable from Settings →
    /// Models; visible verbatim in the Inner Work ProvenancePanel.
    #[serde(default)]
    pub custom_instructions: Option<String>,

    /// Idle seconds before the lazy-loaded primary chat slot is
    /// unloaded to reclaim VRAM. Mirrors
    /// `sovereign_core::setup_config::DaemonSection::primary_idle_secs`
    /// so a desktop user who tunes one expects the other to behave
    /// the same way. Default 300 (5 min) — long enough that
    /// mid-conversation pauses don't re-pay the 10–20s lazy-load
    /// tax on the next turn, short enough that an abandoned session
    /// frees memory within a coffee break. Combined with the
    /// window-focus prewarm, raising this towards "never" gives
    /// always-hot semantics at the cost of pinning ~28 GB for a
    /// 35B Q6.
    #[serde(default = "default_primary_idle_secs")]
    pub primary_idle_secs: u64,

    /// Model family of the embed slot. Controls pooling strategy (mean /
    /// last-token / cls) and instruction prefixes. For most open-weights
    /// embedding models this should be:
    ///   - `Qwen3Embedding` for qwen3-embedding-* GGUF files (last-token pooling)
    ///   - `Unknown` (default) for mxbai and similar mean-pooling
    ///     embedders
    ///
    /// Getting this wrong does not prevent ingestion but will produce
    /// incompatible vectors if you later try to collaborate with a peer
    /// that has it set correctly.
    #[serde(default)]
    pub embed_family: ModelFamily,

    /// Display name used for this node when creating or joining a
    /// mesh — shows up in other members' mesh rosters. Empty string
    /// means "use the system hostname at join time". The user can
    /// override this from Settings → Mesh; changes take effect on
    /// the next mesh create/join, not retroactively (existing
    /// `MemberRecord`s stay put until that member rejoins).
    #[serde(default)]
    pub node_name: String,

    /// Whether the `KnowledgeView` landscape-digest layer is active.
    /// When `true` (default), svrnmesh builds + maintains three
    /// enriched views over memories / conversations / notes and
    /// splices their digest into every conversation's system prompt.
    /// When `false`, the feature is skipped at Runtime construction —
    /// svrnmesh behaves exactly as it did before KnowledgeView existed.
    ///
    /// Toggling this requires a desktop restart: the Runtime is built
    /// once at app startup, with or without the landscape-digest
    /// provider. The setting persists across restarts.
    ///
    /// Default: on. Existing configs without the field read as `true`.
    #[serde(default = "default_knowledge_view_enabled")]
    pub knowledge_view_enabled: bool,

    /// Persisted ceiling on how much disk svrnmesh is allowed to use
    /// for corpus storage (sum of `~/.svrnmesh/indexes/*`). `None`
    /// means "compute a sensible default at boot from free disk" —
    /// the desktop's startup hook turns that into a concrete value
    /// (~100 GiB target, scaled down for tighter machines), persists
    /// it back here, and pushes the value to the daemon over
    /// `POST /internal/storage/budget`. Subsequent edits via
    /// Settings → Knowledge keep both this field and the running
    /// daemon in sync.
    ///
    /// The actual enforcement happens in
    /// `sovereign-mesh::capabilities::build_local_capabilities` —
    /// this field is the persistence layer; the runtime control is
    /// the AppState atomic the daemon owns.
    #[serde(default)]
    pub storage_budget_bytes: Option<u64>,

    /// Result of the first-mesh-join consent dialog (W4). `None`
    /// means the dialog has not yet been shown — the App router
    /// gates the main UI on this. `Some(_)` is the user's decision;
    /// the desktop calls /internal/contribution/ceiling on every
    /// boot with the corresponding peer-inflight cap so the daemon
    /// matches their preference even if its in-memory state was
    /// reset.
    #[serde(default)]
    pub first_mesh_consent: Option<FirstMeshConsent>,
    /// Installed mesh apps + the permission subset the user granted each
    /// at its consent sheet. The bridge (`crate::meshapp`) enforces these
    /// grants; an app absent here is denied every bridge op (fail-closed).
    #[serde(default)]
    pub meshapp_installs: Vec<crate::meshapp::MeshAppInstall>,
}

/// Persisted result of the W4 consent dialog. Captures both the
/// answer and when it was given — the latter is useful for the
/// "you set this 6 months ago, want to revisit?" prompt the
/// Settings panel can surface later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirstMeshConsent {
    /// User chose to share idle GPU with mesh peers. `false`
    /// translates to `contribution_max_peer_inflight = 0` (peer
    /// inference all 503s — equivalent to
    /// `SOVEREIGN_DISABLE_PEER_INFERENCE=1`).
    pub share_gpu: bool,
    /// Concrete peer-inflight ceiling applied at boot. For Yes,
    /// default to 1 (one concurrent peer request — matches the
    /// plan's 25% bucket). Stored explicitly so the user can later
    /// edit it in Settings without re-prompting the consent dialog.
    pub ceiling: usize,
    pub recorded_at_unix: i64,
}

fn default_knowledge_view_enabled() -> bool {
    true
}

fn default_auto_collaborate() -> bool {
    true
}

fn default_naked_mode() -> bool {
    false
}

/// Default 5 min — see the field doc for the rationale.
fn default_primary_idle_secs() -> u64 {
    300
}

/// Resolve the node name the user sees in others' mesh rosters.
/// Preference order: explicit config override → system hostname
/// (via the `hostname` crate, which wraps `gethostname(2)` on
/// Unix and `GetComputerNameW` on Windows) → literal fallback.
/// The hostname path handles macOS properly, where `HOSTNAME`
/// env var isn't exported to launched apps.
pub fn resolve_node_name(configured: &str) -> String {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    match hostname::get() {
        Ok(os) => os
            .into_string()
            .ok()
            // Strip `.local` Bonjour suffix so "Alexs-MBP.local"
            // renders as "Alexs-MBP" in the roster.
            .map(|s| {
                s.strip_suffix(".local")
                    .map(|trimmed| trimmed.to_string())
                    .unwrap_or(s)
            })
            .unwrap_or_else(|| "sovereign-node".to_string()),
        Err(_) => "sovereign-node".to_string(),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchBackendConfig {
    #[serde(default = "default_search_provider")]
    pub provider: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_data_dir() -> PathBuf {
    // THE SSOT accessor (`rebrand::data_dir`), whose own doc says "read sites
    // must not re-derive it". This returned `mesh_data_dir()` (the platform
    // data dir) until 2026-08-24, so a FRESH install put its data in
    // `~/Library/Application Support/svrnmesh` while the daemon used
    // `~/.svrnmesh` — which is how that directory's stale 15G was created.
    // `desktop.toml` is unaffected: it is a settings file and still resolves
    // through `mesh_config_dir()`, which is deliberately platform-native.
    sovereign_contracts::rebrand::data_dir()
}

fn default_skills_dir() -> PathBuf {
    // Note: the path keyword stays `skills` for back-compat with any
    // user-overlay TOMLs already on disk. The bundled in-repo
    // directory is now `modes/` (only inner-work + recipe-author),
    // but the user-overlay slot is unchanged so existing custom
    // skill files still load.
    sovereign_contracts::rebrand::data_dir().join("skills")
}

fn default_enabled_tools() -> Vec<String> {
    vec![
        "shell".to_string(),
        "search".to_string(),
        "web_fetch".to_string(),
        "document".to_string(),
        // Tool-Mastery Phase 5 — unified knowledge front door
        // (corpus + memory + notes). Default-on so the desktop's
        // skill-narrowed catalogs (codebase-navigator, research-
        // analyst, etc.) actually expose it; skills' ToolPreferences
        // can drop it explicitly when not needed.
        "knowledge_lookup".to_string(),
    ]
}

fn default_temperature() -> f32 {
    0.7
}

fn default_max_tokens() -> u32 {
    2048
}

fn default_think_budget() -> u32 {
    512
}

fn default_search_provider() -> String {
    "duckduckgo".to_string()
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            code_family: ModelFamily::Unknown,
            data_dir: default_data_dir(),
            skills_dir: default_skills_dir(),
            active_skills: Vec::new(),
            enabled_tools: default_enabled_tools(),
            search_backend: SearchBackendConfig::default(),
            setup_complete: false,
            selected_tier: None,
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            think_budget: default_think_budget(),
            top_k: None,
            auto_collaborate: default_auto_collaborate(),
            naked_mode: default_naked_mode(),
            shared_model_role: sovereign_core::setup_config::SharedModelRole::default(),
            shared_model_id: None,
            custom_instructions: None,
            primary_idle_secs: default_primary_idle_secs(),
            embed_family: ModelFamily::Unknown,
            node_name: String::new(),
            knowledge_view_enabled: default_knowledge_view_enabled(),
            storage_budget_bytes: None,
            enable_recipe_authoring: false,
            mobile_access_enabled: false,
            auto_escalate_to_web: false,
            first_mesh_consent: None,
            meshapp_installs: Vec::new(),
        }
    }
}

/// Marker file recording that the friendly-name first-launch
/// generator has run for this user. Held next to `desktop.toml`
/// so wiping config also resets the sentinel — letting the user
/// recover a fresh suggestion by deleting both files.
fn sentinel_path() -> PathBuf {
    sovereign_contracts::rebrand::mesh_config_dir().join(".first-name-generated")
}

impl DesktopConfig {
    pub fn config_path() -> PathBuf {
        sovereign_contracts::rebrand::mesh_config_dir().join("desktop.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        let raw: Option<String> = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => Some(content),
                Err(e) => {
                    tracing::warn!("Failed to read config: {e}");
                    None
                }
            }
        } else {
            None
        };
        let mut config: DesktopConfig = match raw.as_deref() {
            Some(content) => match toml::from_str(content) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to parse config: {e}");
                    Self::default()
                }
            },
            None => Self::default(),
        };

        // Migration: copy any pre-merge `context_size` from
        // `desktop.toml` into the canonical home in
        // `~/.svrnmesh/config.toml`'s `[models].context_size`. Runs
        // exactly when:
        //   1. desktop.toml carries an explicit `context_size` AND
        //   2. SetupConfig either doesn't exist OR has no explicit
        //      `context_size` of its own.
        //
        // Goal: a user who set 8192 in the desktop UI before the
        // single-source-of-truth merge keeps that value automatically
        // — the daemon config is created (or amended) with their
        // chosen ctx. After migration the desktop field is moot;
        // bootstrap reads SetupConfig.
        //
        // We do NOT rewrite desktop.toml to clear the field — the
        // deprecated marker on the struct field handles future-proofing
        // and avoiding a config rewrite keeps the migration a pure
        // one-way write into SetupConfig (the worse error mode is
        // surfacing a stale value, not losing one).
        // One-time migration of legacy `desktop.toml` MODEL PATHS +
        // `context_size` into `SetupConfig`. The struct no longer has these
        // fields, so we re-parse the raw TOML through a deserialize-only
        // shim and, when the daemon config doesn't already carry them, copy
        // them over. Guarantees no existing user loses their configured
        // models when they upgrade to the single-source-of-truth build.
        Self::migrate_legacy_model_fields(raw.as_deref());

        // Friendly first-launch node name. Without this, anyone who
        // never opened the node-name input ends up identified by their
        // raw system hostname ("example-host") in mesh rosters,
        // which is forgettable and easy to mistake for someone else.
        // Generate once and persist; never overwrite a name the user
        // explicitly set, and never re-roll if they later cleared the
        // field on purpose (sentinel guards against that).
        let sentinel = sentinel_path();
        let already_generated = sentinel.exists();
        if config.node_name.trim().is_empty() && !already_generated {
            let suggested = crate::friendly_names::generate(None);
            tracing::info!(
                node_name = %suggested,
                "first-launch friendly node name generated"
            );
            config.node_name = suggested;
            // Persist immediately so the suggestion survives even if
            // the user closes the app before opening MeshSettings.
            // Errors here are non-fatal — the in-memory config is
            // still good for this session.
            if let Err(e) = config.save() {
                tracing::warn!("failed to persist first-launch node name: {e}");
            } else if let Some(parent) = sentinel.parent() {
                let _ = std::fs::create_dir_all(parent);
                if let Err(e) = std::fs::write(&sentinel, b"1") {
                    tracing::warn!("failed to write friendly-name sentinel: {e}");
                }
            }
        }

        config
    }

    /// One-time migration of legacy `desktop.toml` model-slot paths +
    /// `context_size` into `SetupConfig` (`~/.svrnmesh/config.toml`). The
    /// struct no longer carries these fields, so the values are recovered
    /// from the raw TOML via a deserialize-only shim. Runs only when the
    /// legacy file actually carries them AND `SetupConfig` doesn't already
    /// have models/ctx of its own — so an already-migrated (or CLI-
    /// configured) user is never clobbered. Does NOT rewrite `desktop.toml`
    /// (serde drops the now-unknown keys on the next save).
    fn migrate_legacy_model_fields(raw: Option<&str>) {
        use sovereign_core::setup_config::{DataSection, ModelsSection, SetupConfig};

        /// Deserialize-only view of the removed model fields, read straight
        /// from the raw `desktop.toml` so the migration can recover values
        /// the live `DesktopConfig` no longer deserializes.
        #[derive(Deserialize, Default)]
        struct LegacyModelPaths {
            #[serde(default)]
            model_path: Option<PathBuf>,
            #[serde(default)]
            primary_model_path: Option<PathBuf>,
            #[serde(default)]
            embed_model_path: Option<PathBuf>,
            #[serde(default)]
            code_model_path: Option<PathBuf>,
            #[serde(default)]
            context_size: Option<u32>,
        }

        let Some(content) = raw else { return };
        let legacy: LegacyModelPaths = match toml::from_str(content) {
            Ok(l) => l,
            Err(_) => return,
        };
        let has_any_path = legacy.model_path.is_some()
            || legacy.primary_model_path.is_some()
            || legacy.embed_model_path.is_some()
            || legacy.code_model_path.is_some();
        if !has_any_path && legacy.context_size.is_none() {
            return; // nothing legacy to migrate
        }

        let existing = match SetupConfig::load() {
            Ok(c) => Some(c),
            // Absent file → synthesize below. An EXISTING but unparseable
            // config.toml must NOT be treated as absent: synthesizing +
            // saving would silently replace the whole file (daemon ports,
            // iroh, watched_folders, …) with defaults. Skip the migration
            // and let the user fix the file instead.
            Err(e) => {
                if SetupConfig::exists() {
                    tracing::warn!(
                        error = %e,
                        "config migration: config.toml exists but is unparseable — \
                         skipping legacy model-path migration rather than overwriting it"
                    );
                    return;
                }
                None
            }
        };
        // Primary alone decides path authority. Requiring embed too would
        // let a stale desktop.toml clobber a CLI-configured config.toml
        // that simply has no embed model — repeatedly, since desktop.toml
        // is never rewritten here (the exact divergence bug this
        // single-source-of-truth work exists to kill).
        let setup_has_models = existing
            .as_ref()
            .is_some_and(|c| !c.models.primary.as_os_str().is_empty());
        let setup_has_ctx = existing
            .as_ref()
            .is_some_and(|c| c.models.context_size.is_some());
        // Nothing to do if SetupConfig is already authoritative for
        // whatever the legacy file could contribute.
        let need_paths = has_any_path && !setup_has_models;
        let need_ctx = legacy.context_size.is_some() && !setup_has_ctx;
        if !need_paths && !need_ctx {
            return;
        }

        let mut setup = existing.unwrap_or_else(|| SetupConfig {
            compute: Default::default(),
            models: ModelsSection {
                primary: PathBuf::new(),
                fast: None,
                embed: PathBuf::new(),
                code: None,
                context_size: None,
                fast_context_size: None,
                extra: std::collections::BTreeMap::new(),
                max_extras_memory_gb: None,
                primary_pool: None,
                edit: None,
            },
            daemon: Default::default(),
            data: DataSection {
                dir: default_data_dir(),
            },
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        });

        if need_paths {
            // Same subsume mapping the old desktop code used: primary is the
            // anchor (primary_model_path, or model_path when only one was
            // set); fast is the distinct model_path, else subsumed (None).
            let primary = legacy
                .primary_model_path
                .clone()
                .or_else(|| legacy.model_path.clone())
                .unwrap_or_default();
            setup.models.fast = match legacy.model_path.clone() {
                Some(f) if !f.as_os_str().is_empty() && f != primary => Some(f),
                _ => None,
            };
            setup.models.primary = primary;
            if let Some(e) = legacy.embed_model_path.clone() {
                setup.models.embed = e;
            }
            // Only carry a code slot the legacy file actually has —
            // a legacy None must not wipe one already in SetupConfig.
            if let Some(c) = legacy.code_model_path.clone() {
                setup.models.code = Some(c);
            }
        }
        if need_ctx {
            setup.models.context_size = legacy.context_size;
        }

        match setup.save() {
            Ok(path) => tracing::info!(
                target = %path.display(),
                migrated_paths = need_paths,
                migrated_ctx = need_ctx,
                "config migration: copied legacy desktop.toml model paths / context_size into SetupConfig"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "config migration: failed to write migrated SetupConfig"
            ),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {e}"))?;
        }
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("Failed to serialize config: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("Failed to write config: {e}"))?;
        Ok(())
    }
}

/// The model-slot paths resolved from `SetupConfig`
/// (`~/.svrnmesh/config.toml`) — the single on-disk source of truth for
/// which GGUFs each slot loads. Bootstrap builds one of these; the
/// CPU-compat policy may mutate it **in memory only** (never persisted);
/// the inference builder loads from it.
///
/// Deliberately distinct from [`DesktopConfig`]: model *paths* live here
/// (in `config.toml`, shared with the daemon), while model *family* hints
/// (`code_family` / `embed_family`) stay on `DesktopConfig` — see those
/// field docs for the split. Keeping paths in exactly one file is what
/// stops the Settings panel and the daemon from ever disagreeing.
#[derive(Debug, Clone)]
pub struct ResolvedModelSlots {
    /// Always-resident quick-responder ("fast") slot. Falls back to the
    /// primary GGUF when no distinct fast model is configured
    /// (`ModelsSection::fast_path` subsume rule).
    pub fast: PathBuf,
    /// Lazy thoughtful ("primary") slot. `Some` whenever a primary is
    /// configured (the common case); the CPU-compat policy may set it to
    /// `None` at runtime when the configured primary can't run here and
    /// no dense substitute exists.
    pub primary: Option<PathBuf>,
    /// Embedding model. Empty path when none is configured yet (fresh
    /// install before setup) — test with [`Self::has_embed`].
    pub embed: PathBuf,
    /// Optional code specialist, hot-swapped into the primary slot.
    pub code: Option<PathBuf>,
    /// Effective context window PER SLOT. Was a single `context_size: u32`
    /// until 2026-08-25, which is precisely how the fast slot came to carry
    /// the primary's 64k window: one resolved scalar, four contexts built
    /// from it, and KV linear in the window.
    pub windows: sovereign_inference::embedded::SlotWindows,
}

impl ResolvedModelSlots {
    /// Read from the canonical `SetupConfig`. Errors when `config.toml` is
    /// absent or unparseable — callers that must tolerate a pre-setup
    /// machine use [`Self::load_or_default`].
    pub fn load() -> Result<Self, String> {
        let setup = sovereign_core::setup_config::SetupConfig::load()?;
        Ok(Self::from_setup(&setup))
    }

    /// Read from `SetupConfig`, or an all-empty placeholder when
    /// `config.toml` doesn't exist yet (fresh install, pre-wizard). Never
    /// errors.
    pub fn load_or_default() -> Self {
        match sovereign_core::setup_config::SetupConfig::load() {
            Ok(setup) => Self::from_setup(&setup),
            Err(_) => Self {
                fast: PathBuf::new(),
                primary: None,
                embed: PathBuf::new(),
                code: None,
                windows: sovereign_inference::embedded::SlotWindows::uniform(16_384),
            },
        }
    }

    fn from_setup(setup: &sovereign_core::setup_config::SetupConfig) -> Self {
        let m = &setup.models;
        Self {
            fast: m.fast_path().to_path_buf(),
            primary: Some(m.primary.clone()),
            embed: m.embed.clone(),
            code: m.code.clone(),
            windows: sovereign_inference::embedded::SlotWindows::from_models(m),
        }
    }

    /// True when an embedding model is actually configured (non-empty path).
    pub fn has_embed(&self) -> bool {
        !self.embed.as_os_str().is_empty()
    }
}
