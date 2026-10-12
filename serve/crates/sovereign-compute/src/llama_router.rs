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
pub mod live;
pub mod provider;
use sovereign_contracts::engine_config::{EmbedInputs, EngineKind, EngineSection, GrammarSupport};
use sovereign_contracts::setup_config::{DaemonSection, ModelsSection};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterPreset {
    entries: BTreeMap<String, RouterEntry>,
    primary: String,
    fast: Option<String>,
    code: Option<String>,
    embed: Option<String>,
    rerank: Option<String>,
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
        if models.edit.is_some() {
            return Err(
                "[models.edit] is not served by kind = \"llama-server\" yet (its route \
                        is step 4 of the engine swap); remove it or run kind = \"llama\""
                    .into(),
            );
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
        let fast = match models.has_explicit_fast() {
            true => Some(chat(
                models.fast_path(),
                models.effective_fast_context_size(),
                Some(FAST_PARALLEL),
                idle_window(idle.fast_idle_secs),
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
            primary,
            fast,
            code,
            embed,
            rerank,
        })
    }

    /// Each slot role this preset fills and the model that fills it, in the
    /// roles the embedded engine reports residency under (`primary`,
    /// `fast`, `code`, `embed`, the rerank kind's role). Two roles on one
    /// GGUF name the same model.
    pub fn roles(&self) -> Vec<(&'static str, &str)> {
        [
            ("primary", Some(&self.primary)),
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

    /// The model that fills the primary role.
    pub fn primary(&self) -> &str {
        &self.primary
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

    /// The models that fit together, to warm and to set `--models-max` by:
    /// the longest run, in the order the embedded preflight sizes its slots
    /// (primary, fast, embed, code, then the reranker), whose working sets
    /// `fits` admits (`capacity::check_fit` in production, so the router is
    /// sized by the one verdict the preflight uses). The rest load on
    /// demand, the router evicting the least recently used. Never empty: a
    /// primary too big on its own is still tried, as the embedded engine
    /// starts on an advisory overcommit.
    pub fn models_that_fit(
        &self,
        fits: impl Fn(&[sovereign_inference::capacity::SlotPlan]) -> bool,
    ) -> Vec<&str> {
        let mut order: Vec<&str> = Vec::new();
        for name in [
            Some(&self.primary),
            self.fast.as_ref(),
            self.embed.as_ref(),
            self.code.as_ref(),
            self.rerank.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if !order.contains(&name.as_str()) {
                order.push(name);
            }
        }
        let mut slots = Vec::new();
        let mut fitting = 0;
        for name in &order {
            let e = &self.entries[*name];
            slots.push(sovereign_inference::capacity::SlotPlan {
                role: name.to_string(),
                path: e.path.clone(),
                // One KV pool per section (kv-unified), `ctx-size` long.
                n_seq_max: 1,
                n_ctx: e.ctx,
            });
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
        let context_size = [Some(&self.primary), self.fast.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(ctx_of)
            .min()
            .unwrap_or(ENCODER_CTX);
        EngineSection {
            kind: EngineKind::Remote,
            endpoint: Some(endpoint_v1.to_string()),
            model_id: Some(self.primary.clone()),
            fast_model_id: self.fast.clone(),
            embed_model_id: self.embed.clone(),
            context_size,
            // llama-server embeds what it is sent; the client prepares the
            // inputs from the model's EmbedQuirks, as the engine did.
            embed_inputs: Some(EmbedInputs::Client),
            // The shipped router is built with llguidance.
            grammar: Some(GrammarSupport::Llguidance),
            ..Default::default()
        }
    }
}

#[cfg(test)]
#[path = "llama_router_tests.rs"]
mod tests;
