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
use sovereign_contracts::setup_config::ModelsSection;

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
}

/// The router's models and which of them fills each slot role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterPreset {
    entries: BTreeMap<String, RouterEntry>,
    primary: String,
    fast: Option<String>,
    embed: Option<String>,
}

fn stem(path: &Path) -> Result<String, String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{} has no file stem to name it by", path.display()))
}

impl RouterPreset {
    /// Build the preset from `[models]` and the rerank kind's GGUF.
    /// `mtp_draft_layers` answers how many draft layers a GGUF declares
    /// (`gguf_meta::read_mtp_draft_layers` in production); an unreadable
    /// GGUF refuses the preset, since the router could not load it either.
    pub fn from_models(
        models: &ModelsSection,
        rerank: Option<PathBuf>,
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
        let mut chat = |path: &Path, ctx: u32, parallel: Option<u32>| -> Result<String, String> {
            let name = stem(path)?;
            let layers = mtp_draft_layers(path)
                .map_err(|e| format!("reading {} for its MTP draft layers: {e}", path.display()))?;
            let mtp_draft = layers.is_some_and(|n| n > 0);
            tracing::debug!(target: "llama_router", model = %name, ?layers, mtp_draft, ctx, "preset: chat entry");
            match entries.get_mut(&name) {
                // Two roles on one GGUF share its entry, at the larger
                // context and the larger slot count.
                Some(RouterEntry {
                    ctx: c,
                    kind: EntryKind::Chat { parallel: p, .. },
                    ..
                }) => {
                    *c = (*c).max(ctx);
                    *p = (*p).max(parallel);
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
                        },
                    );
                }
            }
            Ok(name)
        };
        let primary = chat(&models.primary, models.effective_context_size(), None)?;
        let fast = match models.has_explicit_fast() {
            true => Some(chat(
                models.fast_path(),
                models.effective_fast_context_size(),
                Some(FAST_PARALLEL),
            )?),
            false => None,
        };
        if let Some(code) = &models.code {
            chat(code, models.effective_context_size(), None)?;
        }
        let mut encoder = |path: &Path, kind: EntryKind| -> Result<String, String> {
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
                },
            );
            Ok(name)
        };
        let embed = match models.embed.as_os_str().is_empty() {
            true => None,
            false => Some(encoder(&models.embed, EntryKind::Embed)?),
        };
        if let Some(path) = rerank {
            encoder(&path, EntryKind::Rerank)?;
        }
        Ok(Self {
            entries,
            primary,
            fast,
            embed,
        })
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
