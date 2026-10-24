// SPDX-License-Identifier: AGPL-3.0-or-later
//! The llama-server router a `kind = "llama-server"` node runs: which GGUFs
//! it serves and how, derived from `[models]` and nothing else.
//!
//! The preset is the router's `--models-preset` file, one section per GGUF
//! named by its stem, which is also the model id the remote provider sends.
//! Everything the remote half needs — endpoint, ids, `embed_inputs`,
//! `grammar` — follows from the preset and the router's port
//! ([`RouterPreset::engine_section`]); none of it is configured.
//!
//! The settings were watched working in the conformance battery
//! (`bench/lanes/engine-swap/battery.sh`, svrn-docs
//! ENGINE_CONFORMANCE_ISSUES.md C16); the plan is svrn-docs
//! ENGINE_SWAP_PLAN.md, step 1.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub mod build_pin;
mod edit;
pub mod live;
pub mod placement;
pub mod provider;
use sovereign_contracts::engine_config::{EmbedInputs, EngineKind, EngineSection, GrammarSupport};
use sovereign_contracts::setup_config::{DaemonSection, ModelsSection};

use self::placement::RouterPlacement;
use sovereign_inference::capacity::SlotPlan;

/// Draft tokens per MTP step: the embedded engine's default
/// (`mtp_draft_max_decide`), the value every battery pass ran.
const MTP_DRAFT_N_MAX: u32 = 3;

/// Parallel sequences on the fast model: the fast lane's coalescer sends 8
/// at once, and on the server's default 4 they ran in two waves (C16: 52.6
/// tok/s against embedded 68.8; with 8, 72.6 and 74.5).
const FAST_PARALLEL: u32 = 8;

/// Context, batch and micro-batch for the embedding and rerank models. An
/// encoder needs a whole input in one micro-batch; 8192 is the battery's
/// configuration (compute.embeddings 9/9).
const ENCODER_CTX: u32 = 8192;

/// Server slots on the rerank section. The spike scored its pairs on a server
/// with `-np 1` and took 2x the embedded engine's wall time (R4, 2026-10-12):
/// the embedded slot fans a shared prefix out to 16 document sequences, while
/// a one-slot server runs every document alone. Slots that can overlap are
/// what recovers that fan-out (A4; R4 re-measured 2026-10-23).
const RERANK_PARALLEL: u32 = 4;

/// What one router entry serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A chat model. `mtp_draft` when the GGUF declares draft layers;
    /// `parallel` when it needs more slots than the server's default.
    Chat {
        /// `spec-type = draft-mtp`.
        mtp_draft: bool,
        /// `parallel`, where it differs from the server's default.
        parallel: Option<u32>,
    },
    /// The embedding model.
    Embed,
    /// The reranker (`--reranking`).
    Rerank,
}

/// One `[section]` of the preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterEntry {
    /// The GGUF.
    pub path: PathBuf,
    /// `ctx-size`.
    pub ctx: u32,
    /// What it serves.
    pub kind: EntryKind,
    /// `sleep-idle-seconds`: the `[daemon] *_idle_secs` of the roles it
    /// fills. `None` never sleeps.
    pub sleep_idle_secs: Option<u64>,
}

/// One role's `[daemon] *_idle_secs` as a sleep window: 0 pins the model,
/// which the server spells by leaving the flag out (it refuses 0).
fn idle_window(secs: u64) -> Option<u64> {
    (secs > 0).then_some(secs)
}

/// Two roles on one GGUF hold one copy of the weights, which is freed only
/// once both would have let it go: the longer window, and never if either
/// is pinned.
fn shared_window(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    a.zip(b).map(|(a, b)| a.max(b))
}

/// The router's models and which of them fills each slot role.
///
/// `primary` is `None` for an embed-only router: a hosted (remote) node that
/// embeds in this process ([`RouterPreset::embed_only`]) runs a router that
/// serves its embedding model and no chat model at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterPreset {
    entries: BTreeMap<String, RouterEntry>,
    primary: Option<String>,
    fast: Option<String>,
    code: Option<String>,
    /// `[models.edit]`'s model.
    edit: Option<String>,
    embed: Option<String>,
    rerank: Option<String>,
    /// Where its models run: [`RouterPlacement::Local`] for every preset
    /// until serving assembly attaches the config's peers
    /// ([`Self::with_placement`]), so a preset built on its own — every
    /// unit test, the hosted pair's one section — renders the one-device
    /// server it always did.
    placement: RouterPlacement,
}

fn stem(path: &Path) -> Result<String, String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{} has no file stem to name it by", path.display()))
}

impl RouterPreset {
    /// Build the preset from `[models]`, the rerank kind's GGUF and the
    /// `[daemon] *_idle_secs` windows the embedded engine unloads on (code
    /// shares the primary's lazy slot there, so its window; the reranker
    /// has none). `mtp_draft_layers` answers how many draft layers a GGUF
    /// declares (`gguf_meta::read_mtp_draft_layers` in production); an
    /// unreadable GGUF refuses the preset, since the router could not load
    /// it either.
    pub fn from_models(
        models: &ModelsSection,
        rerank: Option<PathBuf>,
        idle: &DaemonSection,
        mtp_draft_layers: impl Fn(&Path) -> std::io::Result<Option<u32>>,
    ) -> Result<Self, String> {
        if !models.extra.is_empty() {
            return Err(format!(
                "[models.extra] ({}) is not served by kind = \"llama-server\" yet; \
                 remove it or run kind = \"llama\"",
                models.extra.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        let mut entries: BTreeMap<String, RouterEntry> = BTreeMap::new();
        let mut chat = |path: &Path,
                        ctx: u32,
                        parallel: Option<u32>,
                        sleep: Option<u64>|
         -> Result<String, String> {
            let name = stem(path)?;
            let layers = mtp_draft_layers(path)
                .map_err(|e| format!("reading {} for its MTP draft layers: {e}", path.display()))?;
            let mtp_draft = layers.is_some_and(|n| n > 0);
            tracing::debug!(target: "llama_router", model = %name, ?layers, mtp_draft, ctx, ?sleep, "preset: chat entry");
            match entries.get_mut(&name) {
                // Two roles on one GGUF share its entry, at the larger
                // context, the larger slot count and the shared window.
                Some(RouterEntry {
                    ctx: c,
                    kind: EntryKind::Chat { parallel: p, .. },
                    sleep_idle_secs: s,
                    ..
                }) => {
                    *c = (*c).max(ctx);
                    *p = (*p).max(parallel);
                    *s = shared_window(*s, sleep);
                }
                Some(other) => {
                    return Err(format!("{name} is both a chat model and {:?}", other.kind));
                }
                None => {
                    entries.insert(
                        name.clone(),
                        RouterEntry {
                            path: path.to_path_buf(),
                            ctx,
                            kind: EntryKind::Chat {
                                mtp_draft,
                                parallel,
                            },
                            sleep_idle_secs: sleep,
                        },
                    );
                }
            }
            Ok(name)
        };
        let primary_sleep = idle_window(idle.primary_idle_secs);
        let primary = chat(
            &models.primary,
            models.effective_context_size(),
            None,
            primary_sleep,
        )?;
        let fast_sleep = idle_window(idle.fast_idle_secs);
        let fast = match models.has_explicit_fast() {
            true => Some(chat(
                models.fast_path(),
                models.effective_fast_context_size(),
                Some(FAST_PARALLEL),
                fast_sleep,
            )?),
            false => None,
        };
        let code = match &models.code {
            Some(path) => Some(chat(
                path,
                models.effective_context_size(),
                None,
                primary_sleep,
            )?),
            None => None,
        };
        // The editing model is a chat section at its own context, pinned as
        // the embedded engine pins its edit slot; on a GGUF another role
        // already serves it shares that section and its window, as the
        // embedded engine aliases the fast slot.
        let edit = match &models.edit {
            Some(section) => {
                let name = Some(stem(&section.path)?);
                let sleep = match () {
                    _ if name == fast => fast_sleep,
                    _ if name.as_ref() == Some(&primary) || name == code => primary_sleep,
                    _ => None,
                };
                Some(chat(
                    &section.path,
                    section.effective_context_size(),
                    None,
                    sleep,
                )?)
            }
            None => None,
        };
        let mut encoder =
            |path: &Path, kind: EntryKind, sleep: Option<u64>| -> Result<String, String> {
                let name = stem(path)?;
                if entries.contains_key(&name) {
                    return Err(format!("{name} is named twice in [models]"));
                }
                entries.insert(
                    name.clone(),
                    RouterEntry {
                        path: path.to_path_buf(),
                        ctx: ENCODER_CTX,
                        kind,
                        sleep_idle_secs: sleep,
                    },
                );
                Ok(name)
            };
        let embed = match models.embed.as_os_str().is_empty() {
            true => None,
            false => Some(encoder(
                &models.embed,
                EntryKind::Embed,
                idle_window(idle.embed_idle_secs),
            )?),
        };
        let rerank = match rerank {
            Some(path) => Some(encoder(&path, EntryKind::Rerank, None)?),
            None => None,
        };
        Ok(Self {
            entries,
            primary: Some(primary),
            fast,
            code,
            edit,
            embed,
            rerank,
            placement: RouterPlacement::Local,
        })
    }

    /// The preset a hosted (remote) node embeds through: its `[engine]
    /// embed_path` as the one section, and no chat model at all — generation
    /// stays at the vendor (ENGINE_SWAP_PLAN A6). The embed section is built
    /// exactly as [`Self::from_models`] builds it, so a hosted node and a
    /// router-serving one embed the same way.
    pub fn embed_only(path: &Path, idle: &DaemonSection) -> Result<Self, String> {
        let name = stem(path)?;
        let mut entries = BTreeMap::new();
        entries.insert(
            name.clone(),
            RouterEntry {
                path: path.to_path_buf(),
                ctx: ENCODER_CTX,
                kind: EntryKind::Embed,
                sleep_idle_secs: idle_window(idle.embed_idle_secs),
            },
        );
        Ok(Self {
            entries,
            primary: None,
            fast: None,
            code: None,
            edit: None,
            embed: Some(name),
            rerank: None,
            placement: RouterPlacement::Local,
        })
    }

    /// Each slot role this preset fills and the model that fills it, in the
    /// roles the embedded engine reports residency under (`primary`,
    /// `fast`, `code`, `embed`, the rerank kind's role). Two roles on one
    /// GGUF name the same model.
    pub fn roles(&self) -> Vec<(&'static str, &str)> {
        [
            ("primary", self.primary.as_ref()),
            ("fast", self.fast.as_ref()),
            ("code", self.code.as_ref()),
            ("embed", self.embed.as_ref()),
            (
                sovereign_inference::served_kind::RERANK.role,
                self.rerank.as_ref(),
            ),
        ]
        .into_iter()
        .filter_map(|(role, name)| name.map(|n| (role, n.as_str())))
        .collect()
    }

    /// `[models.edit]`'s model, when one is configured.
    pub fn edit(&self) -> Option<&str> {
        self.edit.as_deref()
    }

    /// Where this preset's models run. [`RouterPlacement::Local`] is the
    /// single-card server; a distributed one carries `[compute] rpc_peers`
    /// and writes them into every section (es-f2).
    pub fn placement(&self) -> &RouterPlacement {
        &self.placement
    }

    /// `self` with `[compute] rpc_peers` as the placement — the one way a
    /// preset becomes distributed. Serving assembly calls it on the planned
    /// preset, so the digest, the render and the router's argv all come from
    /// one preset (and a reload asking for other peers is a different
    /// digest, refused by name like any other preset change).
    pub fn with_placement(self, placement: RouterPlacement) -> Self {
        Self { placement, ..self }
    }

    /// The model that fills the primary role. `None` on an embed-only router
    /// ([`Self::embed_only`]), which serves no chat model.
    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    /// The GGUF behind a model this preset serves.
    pub fn path_of(&self, model: &str) -> Option<&Path> {
        self.entries.get(model).map(|e| e.path.as_path())
    }

    /// The `--models-preset` file. Deterministic: sections in name order.
    pub fn render(&self) -> String {
        let mut out = String::from("version = 1\n\n[*]\nn-gpu-layers = 99\n");
        for (name, e) in &self.entries {
            let _ = write!(
                out,
                "\n[{name}]\nmodel = {}\nctx-size = {}\n",
                e.path.display(),
                e.ctx
            );
            // A distributed section names its peers BEFORE anything that
            // indexes a device. llama.cpp keeps a preset's options in a map
            // keyed by each option's first spelling, so it applies `--rpc`
            // (registering the RPC devices) before `--tensor-split` on
            // `-t` alone; `arg.cpp:2598` states the same ordering contract
            // for `-mmdev`, which sorts between them. Written here in that
            // order so a reader of the file reads it in the order it takes
            // effect, and so `-ts`'s index 0 is the first peer.
            if let Some(rpc) = self.placement.rpc_list() {
                let _ = writeln!(out, "rpc = {rpc}");
            }
            if let Some(secs) = e.sleep_idle_secs {
                let _ = writeln!(out, "sleep-idle-seconds = {secs}");
            }
            match &e.kind {
                EntryKind::Chat {
                    mtp_draft,
                    parallel,
                } => {
                    // One KV pool shared by every slot: without it, more
                    // slots split the context (C16: 8 slots cut each to
                    // 8192 of 65536).
                    out.push_str("kv-unified = true\n");
                    if let Some(n) = parallel {
                        let _ = writeln!(out, "parallel = {n}");
                    }
                    if *mtp_draft {
                        let _ = write!(
                            out,
                            "spec-type = draft-mtp\nspec-draft-n-max = {MTP_DRAFT_N_MAX}\n"
                        );
                    }
                }
                EntryKind::Embed | EntryKind::Rerank => {
                    let flag = match e.kind {
                        EntryKind::Embed => "embedding",
                        _ => "reranking",
                    };
                    let _ = write!(
                        out,
                        "{flag} = true\nbatch-size = {ENCODER_CTX}\nubatch-size = {ENCODER_CTX}\n"
                    );
                    // A rerank request is one task per document, so the
                    // section's slots are what let them overlap (R4).
                    if matches!(e.kind, EntryKind::Rerank) {
                        let _ = writeln!(out, "parallel = {RERANK_PARALLEL}");
                    }
                }
            }
        }
        out
    }

    /// Identity of the preset, from its rendered text: a reload whose preset
    /// differs is a different router.
    pub fn digest(&self) -> String {
        let hash = Sha256::digest(self.render().as_bytes());
        hash.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }

    /// Every model the router should load at start, in name order.
    pub fn warm_models(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// The sections the embedded preflight sizes, in the order it sizes them
    /// (primary, fast, embed, code, then the reranker), each once however many
    /// roles name it. [`Self::models_that_fit`] and
    /// [`Self::vram_if_every_section_loaded`] share this one list.
    fn sections_in_preflight_order(&self) -> Vec<&str> {
        let mut order: Vec<&str> = Vec::new();
        for name in [
            self.primary.as_ref(),
            self.fast.as_ref(),
            self.embed.as_ref(),
            self.code.as_ref(),
            self.edit.as_ref(),
            self.rerank.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !order.contains(&name.as_str()) {
                order.push(name);
            }
        }
        order
    }

    fn slot_plan(&self, name: &str) -> SlotPlan {
        let e = &self.entries[name];
        SlotPlan {
            role: name.to_string(),
            path: e.path.clone(),
            // One KV pool per section (kv-unified), `ctx-size` long.
            n_seq_max: 1,
            n_ctx: e.ctx,
        }
    }

    /// What the router asks of the card if EVERY section is warm at once.
    ///
    /// This is not [`Self::models_that_fit`], which answers what the card
    /// holds together and is what `--models-max` is set by. It is the bound
    /// on the whole preset: a router configured so one card no longer holds
    /// its models answers with `Err` naming what it needs, rather than
    /// warming what fits and serving the rest through eviction nobody chose.
    pub fn vram_if_every_section_loaded(
        &self,
        fits: impl Fn(&[SlotPlan]) -> bool,
    ) -> Result<(), String> {
        let slots: Vec<_> = self
            .sections_in_preflight_order()
            .into_iter()
            .map(|name| self.slot_plan(name))
            .collect();
        if slots.is_empty() || fits(&slots) {
            return Ok(());
        }
        Err(format!(
            "[models] asks for {} models at once, which one card no longer holds together. \
             A 9B-class primary plus the embedder, the reranker and a code model is the \
             two-box case the router's RPC split serves (es-f2), not one node's \
             preset.\nhint: drop a section, or move the primary to the peer's card.",
            slots.len()
        ))
    }

    /// The models that fit together, to warm and to set `--models-max` by:
    /// the longest run, in the order the embedded preflight sizes its slots
    /// ([`Self::sections_in_preflight_order`]), whose working sets `fits`
    /// admits (`capacity::check_fit` in production, so the router is sized by
    /// the one verdict the preflight uses). The rest load on demand, the
    /// router evicting the least recently used. Never empty: a primary too
    /// big on its own is still tried, as the embedded engine starts on an
    /// advisory overcommit.
    pub fn models_that_fit(&self, fits: impl Fn(&[SlotPlan]) -> bool) -> Vec<&str> {
        let mut order = self.sections_in_preflight_order();
        let mut slots = Vec::new();
        let mut fitting = 0;
        for name in &order {
            slots.push(self.slot_plan(name));
            if !fits(&slots) {
                break;
            }
            fitting += 1;
        }
        tracing::debug!(target: "llama_router", fitting, of = order.len(), "preset: models that fit together");
        order.truncate(fitting.max(1));
        order
    }

    /// The remote half's section for a router listening at `endpoint_v1`.
    /// `context_size` is one value for every chat model, so it is the
    /// smaller of primary and fast: a budget the fast model can also hold.
    pub fn engine_section(&self, endpoint_v1: &str) -> EngineSection {
        let ctx_of = |name: &String| self.entries.get(name).map(|e| e.ctx);
        let context_size = [self.primary.as_ref(), self.fast.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(ctx_of)
            .min()
            .unwrap_or(ENCODER_CTX);
        EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some(endpoint_v1.to_string()),
            model_id: self.primary.clone(),
            fast_model_id: self.fast.clone(),
            embed_model_id: self.embed.clone(),
            context_size,
            // llama-server embeds what it is sent; the client prepares the
            // inputs from the model's EmbedQuirks, as the engine did.
            embed_inputs: Some(EmbedInputs::Client),
            // The shipped router is built with llguidance.
            grammar: Some(GrammarSupport::Llguidance),
            // The embedded engine's decode deadline, on every chat request.
            // The server measures it from the first generated token and
            // stops only at a newline past it, where the engine stopped
            // the wall clock outright: no caller reads that difference.
            extra_params: Some(serde_json::json!({
                "t_max_predict_ms": sovereign_inference::embedded::inference_deadline_secs() * 1000
            })),
            ..Default::default()
        }
    }
}

#[cfg(test)]
#[path = "llama_router_tests.rs"]
mod tests;
