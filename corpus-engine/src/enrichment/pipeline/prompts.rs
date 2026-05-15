//! Runtime prompt-asset loading with compile-time fallback.
//!
//! Pipelines used to bake their prompt MD files at compile time via
//! `include_str!`. That meant any prompt edit forced a sovereign-cli
//! rebuild — minutes of churn per iteration during prompt tuning.
//!
//! This module replaces the bare `include_str!` constants with
//! `LazyLock<&'static str>` cells that consult `$SOVEREIGN_PROMPT_DIR`
//! at first read. When the env var is set AND the file exists at
//! `$SOVEREIGN_PROMPT_DIR/<name>`, the override is loaded (and
//! leaked into the static arena so the existing
//! `&'static str` API surface stays untouched). Otherwise the
//! compile-time-baked string is returned unchanged.
//!
//! Usage at the pipeline level:
//!
//! ```rust,ignore
//! use std::sync::LazyLock;
//! use crate::enrichment::pipeline::prompts::load_or_baked;
//!
//! pub static PHASE1_ATLAS_SYSTEM: LazyLock<&'static str> = LazyLock::new(|| {
//!     load_or_baked(
//!         "literary_atlas/phase1_system.md",
//!         include_str!("literary_atlas_prompts/phase1_system.md"),
//!     )
//! });
//!
//! // Callers
//! fn phase1_system(&self) -> &'static str { *PHASE1_ATLAS_SYSTEM }
//! ```
//!
//! Cost: per-process one-time leak of overlay-loaded prompts when
//! the env var is set. ~40 prompts × ~10 KB each ≈ 400 KB max.
//! Acceptable — these are tuning iterations, not long-running
//! services, and the trade is bounded against a multi-minute
//! `sovereign-cli` rebuild per prompt edit.
//!
//! Iteration workflow:
//!
//! ```bash
//! export SOVEREIGN_PROMPT_DIR=/path/to/prompt/overlays
//! mkdir -p $SOVEREIGN_PROMPT_DIR/literary_atlas
//! cp corpus-engine/src/enrichment/pipeline/pipelines/literary_atlas_prompts/phase1_system.md \
//!    $SOVEREIGN_PROMPT_DIR/literary_atlas/phase1_system.md
//! # edit the overlay copy
//! sovereign enrich build <corpus>   # picks up the edit on next run
//! ```
//!
//! Without the env var set, behaviour is bit-identical to the
//! pre-refactor compile-time-baked version.

use std::path::Path;

/// Environment variable that points at the overlay-prompt root.
/// When set AND the corresponding `<root>/<name>` file exists, the
/// override is loaded. Otherwise the compile-time-baked fallback is
/// returned. Read once per `LazyLock` cell.
pub const OVERLAY_ENV_VAR: &str = "SOVEREIGN_PROMPT_DIR";

/// Resolve a prompt asset by overlay-name, falling back to the
/// compile-time-baked string.
///
/// `name` is the path under the overlay root (slash-separated, no
/// leading slash). By convention it matches the relative path under
/// the pipelines tree but with the trailing `_prompts/` segment
/// dropped — e.g. `literary_atlas/phase1_system.md`. Callers pick a
/// scheme; the resolver doesn't enforce one.
///
/// `baked` is the `include_str!()` result the caller would have
/// returned before this module existed.
pub fn load_or_baked(name: &str, baked: &'static str) -> &'static str {
    let Ok(dir) = std::env::var(OVERLAY_ENV_VAR) else {
        return baked;
    };
    let trimmed = dir.trim();
    if trimmed.is_empty() {
        return baked;
    }
    let path = Path::new(trimmed).join(name);
    if !path.is_file() {
        return baked;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            tracing::debug!(
                target: "enrichment.prompts",
                overlay = %path.display(),
                "loaded overlay prompt"
            );
            Box::leak(text.into_boxed_str())
        }
        Err(e) => {
            tracing::warn!(
                target: "enrichment.prompts",
                overlay = %path.display(),
                error = %e,
                "overlay file exists but read failed; falling back to baked",
            );
            baked
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Tests mutate $SOVEREIGN_PROMPT_DIR; serialise so they don't
    // race when cargo runs them in parallel threads.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn returns_baked_when_env_var_unset() {
        let _g = ENV_GUARD.lock().unwrap();
        std::env::remove_var(OVERLAY_ENV_VAR);
        let out = load_or_baked("any/path.md", "BAKED");
        assert_eq!(out, "BAKED");
    }

    #[test]
    fn returns_baked_when_env_var_empty() {
        let _g = ENV_GUARD.lock().unwrap();
        std::env::set_var(OVERLAY_ENV_VAR, "");
        let out = load_or_baked("any/path.md", "BAKED");
        assert_eq!(out, "BAKED");
        std::env::remove_var(OVERLAY_ENV_VAR);
    }

    #[test]
    fn returns_baked_when_overlay_file_missing() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        std::env::set_var(OVERLAY_ENV_VAR, tmp.path());
        let out = load_or_baked("missing/path.md", "BAKED");
        assert_eq!(out, "BAKED");
        std::env::remove_var(OVERLAY_ENV_VAR);
    }

    #[test]
    fn returns_overlay_when_file_present() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let pipeline_dir = tmp.path().join("test_pipeline");
        fs::create_dir_all(&pipeline_dir).unwrap();
        let asset = pipeline_dir.join("phase1.md");
        fs::write(&asset, "OVERRIDE CONTENT").unwrap();

        std::env::set_var(OVERLAY_ENV_VAR, tmp.path());
        let out = load_or_baked("test_pipeline/phase1.md", "BAKED");
        assert_eq!(out, "OVERRIDE CONTENT");
        std::env::remove_var(OVERLAY_ENV_VAR);
    }

    #[test]
    fn returns_baked_when_overlay_is_directory_not_file() {
        let _g = ENV_GUARD.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let dir_at_asset_path = tmp.path().join("test_pipeline").join("phase1.md");
        fs::create_dir_all(&dir_at_asset_path).unwrap();

        std::env::set_var(OVERLAY_ENV_VAR, tmp.path());
        let out = load_or_baked("test_pipeline/phase1.md", "BAKED");
        assert_eq!(out, "BAKED");
        std::env::remove_var(OVERLAY_ENV_VAR);
    }
}
