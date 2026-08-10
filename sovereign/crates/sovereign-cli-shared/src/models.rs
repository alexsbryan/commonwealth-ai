// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the user's `~/.sovereign/config.toml` says the models are.
//!
//! Reads `SetupConfig` from `sovereign-contracts` (the daemon↔package
//! contract crate — NOT `sovereign-core`, which merely re-exports it and would
//! drag corpus-engine + tokio into every CLI binary).
//!
//! Shared because two binaries now stamp the same label: `project init` ships
//! in `sovereign-cli` since 2026-08-07, while `project serve` stayed in
//! `sovereign-cli-dev`. Both must name the embed model the same way or the
//! corpus metadata disagrees with the daemon that built it.

/// Fallback when the user hasn't run `svrn setup` yet.
const DEFAULT_EMBED_MODEL: &str = "qwen3-embedding-0.6b";

/// The configured embed model's filename stem, lowercased — e.g.
/// `qwen3-embedding-0.6b-q8_0`. This is what `CorpusEngine::with_embedding_model`
/// stamps into a corpus's `_corpus_meta.json`, so the log line and the metadata
/// reflect what was actually loaded rather than the engine's built-in default.
///
/// Falls back to [`DEFAULT_EMBED_MODEL`] when no config exists. That is safe
/// for the callers that use it: they index FTS-only, so the name is a label,
/// not a promise about a vector space. A caller that actually EMBEDS must fail
/// instead of defaulting — see `build_daemon_embed_fn`, which returns `Err`
/// when the stem can't be resolved (ARCH §18.3: never silently substitute).
pub fn configured_embed_model_name() -> String {
    if let Ok(cfg) = sovereign_contracts::setup_config::SetupConfig::load() {
        if let Some(stem) = cfg.models.embed.file_stem().and_then(|s| s.to_str()) {
            return stem.to_lowercase();
        }
    }
    DEFAULT_EMBED_MODEL.to_string()
}
