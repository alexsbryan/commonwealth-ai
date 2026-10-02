// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stream-axis taxonomy — the two orthogonal dimensions every meta-atom
//! anchor is tagged with.
//!
//! The meta-atlas substrate (see `meta_atlas/`) classifies atoms across
//! corpora into a three-stream articulation taxonomy + a three-state
//! stability taxonomy. Together these give retrieval the legibility
//! signal it needs to render the synthesis prompt in stream-separated
//! sections.
//!
//! Two important invariants pinned here:
//!
//! 1. **Articulation is per-atom.** The classifier in
//!    [`crate::meta_atlas::classifier`] runs over every atom in every
//!    installed atlas and emits an [`ArticulationVector`] per atom.
//!    Heterogeneous user corpora (a single Obsidian vault that mixes
//!    journals, essays, and reference cards) get per-atom axis tags
//!    even though the corpus has one recipe and one `_corpus_meta.json`.
//!    Recipe declarations are a fallback hint at best; atom shape is
//!    the substrate.
//!
//! 2. **Stability is per-corpus.** Stability is a property of the
//!    *write contract* — does the corpus accept deltas, is it
//!    watcher-driven, is it a published snapshot? — not of any
//!    individual atom. Derived once at corpus install from
//!    `acquire.kind()` + `update.ingest_driver` and written into
//!    `_corpus_meta.json`. Same value for every atom in a corpus.

// The per-atom articulation types live in the language leaf; re-exported here
// so every historical `crate::stream_axes::{Articulation, ArticulationVector}`
// path keeps resolving.
pub use understanding_vocab::articulation::{Articulation, ArticulationVector}; // shim: moved by domains REVIEW-build-articulation-vocab

/// Stability axis — what temporal contract the corpus carries.
///
/// The per-corpus metadata types the index persists are DEFINED in the
/// `corpus-index` leaf and EMBEDDED here (DE "The read-port leaf, measured
/// again"); the derivation below stays with Ingest.
pub use corpus_index::stream_axes::{Stability, StreamAxes, StreamAxesSource}; // shim: moved by domains REVIEW-build-index-read-port

/// Derive per-corpus stability from observable recipe signals.
///
/// Stability is a property of the *write contract* — does the corpus
/// accept deltas, is it watcher-driven, is it a published snapshot?
/// — not of any individual atom. Same value for every atom in a
/// corpus.
///
/// Rule (in priority order):
///   1. Recipe declares `[update] ingest_driver = "watcher"` →
///      Rolling. The daemon-side watcher is continuously refreshing
///      content within a window.
///   2. `BulkDownload` / `HuggingFaceDataset` acquire → Frozen.
///      These are snapshot releases; re-ingest replaces wholesale.
///   3. `HttpApi` / `LocalFile` / `WebCrawl` / `Custom` acquire →
///      Versioned. These accept deltas under their own update
///      cadence.
///   4. Default (no acquire? legacy meta?) → Versioned, the safe
///      catch-all.
///
/// The signal summary returned alongside the verdict is for the
/// `_corpus_meta.json::stream.from_signal` legibility surface — an
/// operator inspecting the stream block can see exactly which
/// recipe fields drove the derivation.
pub fn derive_stability(
    acquire: &crate::recipe::AcquirerConfig,
    update: Option<&crate::recipe::UpdateConfig>,
) -> (Stability, String) {
    use crate::recipe::AcquirerConfig;
    let driver = update.and_then(|u| u.ingest_driver.as_deref());
    if matches!(driver, Some("watcher")) {
        return (
            Stability::Rolling,
            "update.ingest_driver=watcher".to_string(),
        );
    }
    let (stability, acquire_label) = match acquire {
        AcquirerConfig::BulkDownload { .. } => (Stability::Frozen, "acquire=bulk_download"),
        AcquirerConfig::HuggingFaceDataset { .. } => {
            (Stability::Frozen, "acquire=huggingface_dataset")
        }
        AcquirerConfig::HttpApi { .. } => (Stability::Versioned, "acquire=http_api"),
        AcquirerConfig::LocalFile { .. } => (Stability::Versioned, "acquire=local_file"),
        AcquirerConfig::WebCrawl { .. } => (Stability::Versioned, "acquire=web_crawl"),
        AcquirerConfig::Custom { .. } => (Stability::Versioned, "acquire=custom"),
    };
    let driver_label = driver
        .map(|d| format!(", update.ingest_driver={d}"))
        .unwrap_or_default();
    (stability, format!("{acquire_label}{driver_label}"))
}

/// Current unix-seconds timestamp. Helper used by callers building a
/// fresh [`StreamAxes`] block. Mirrors the timestamp shape
/// `IndexMeta::created_at` / `last_updated` use elsewhere in the
/// crate.
pub use corpus_engine_yield::time::unix_now_u64 as timestamp_now;

/// Best-effort derivation from an [`crate::types::IndexInfo`] alone,
/// without needing to parse the recipe file. Used by
/// `sovereign corpus stream-axes` to backfill the block on installed
/// corpora when the recipe isn't readily available.
///
/// Signals consulted (in priority order):
///   1. `parent_corpus_id` matches a known watcher-driven layer
///      (`*-newsworthy`) → Rolling.
///   2. Conversation-history-shaped corpus_id → Rolling.
///   3. Catalog kind → Frozen (catalogs are snapshot inventories).
///   4. Code kind or `source_path` present → Versioned (local file
///      watch).
///   5. `update_manifest_url` present → Versioned (HTTP-driven
///      delta cadence).
///   6. Default → Frozen (the most common bulk-download case).
///
/// Returns the verdict + signal summary for the
/// `from_signal` legibility surface.
pub fn derive_stability_from_info(info: &crate::types::IndexInfo) -> (Stability, String) {
    if let Some(parent) = info.parent_corpus_id.as_deref() {
        if parent.contains("newsworthy") {
            return (Stability::Rolling, format!("parent_corpus_id={parent}"));
        }
    }
    if info.corpus_id.starts_with("conversation")
        || info.corpus_id.contains("history")
        || info.corpus_id.contains("codex-session")
    {
        return (Stability::Rolling, format!("corpus_id={}", info.corpus_id));
    }
    // Watched-folder corpora: ids stamped by the watcher tooling
    // (`watched-…`, `folder-…`, `obsidian-…` — since 2026-06-11 a
    // readable slug sits between kind prefix and hash). Live
    // local-file content; the user keeps editing files. Versioned by
    // definition. `obsidian-` was missing here before — vaults fell
    // through to the Knowledge-kind default.
    if info.corpus_id.starts_with("watched-")
        || info.corpus_id.starts_with("folder-")
        || info.corpus_id.starts_with("obsidian-")
    {
        return (
            Stability::Versioned,
            format!("corpus_id={}", info.corpus_id),
        );
    }
    match info.kind {
        crate::types::CorpusKind::Catalog => (Stability::Frozen, "kind=catalog".to_string()),
        crate::types::CorpusKind::Code => (Stability::Versioned, "kind=code".to_string()),
        crate::types::CorpusKind::Knowledge => {
            if info.update_manifest_url.is_some() {
                (
                    Stability::Versioned,
                    "kind=knowledge, update_manifest_url=present".to_string(),
                )
            } else {
                (
                    Stability::Frozen,
                    "kind=knowledge, no_update_manifest".to_string(),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_as_str_matches_serde_snake_case() {
        assert_eq!(Stability::Frozen.as_str(), "frozen");
        assert_eq!(Stability::Versioned.as_str(), "versioned");
        assert_eq!(Stability::Rolling.as_str(), "rolling");
    }

    // ── derive_stability ───────────────────────────────────

    mod stability_tests {
        use super::*;
        use crate::recipe::{AcquirerConfig, UpdateConfig};

        fn bulk() -> AcquirerConfig {
            AcquirerConfig::BulkDownload {
                url: Some("https://example.com/x.zip".into()),
                urls: None,
                resume: true,
            }
        }

        fn local() -> AcquirerConfig {
            AcquirerConfig::LocalFile {
                path: "/tmp/x".into(),
            }
        }

        fn http_api() -> AcquirerConfig {
            AcquirerConfig::HttpApi {
                base_url: "https://example.com".into(),
                requests: Vec::new(),
                pagination: None,
                follow: None,
                rate_limit_per_second: None,
                user_agent: None,
                headers: None,
            }
        }

        fn hf() -> AcquirerConfig {
            AcquirerConfig::HuggingFaceDataset {
                repo: "org/dataset".into(),
                subset: None,
                file_indices: None,
            }
        }

        fn watcher_update() -> UpdateConfig {
            UpdateConfig {
                manifest_url: "".into(),
                auto_update: false,
                ingest_driver: Some("watcher".into()),
            }
        }

        #[test]
        fn watcher_overrides_acquire_to_rolling() {
            let (s, sig) = derive_stability(&bulk(), Some(&watcher_update()));
            assert_eq!(s, Stability::Rolling);
            assert!(sig.contains("watcher"));
        }

        #[test]
        fn bulk_download_is_frozen() {
            let (s, sig) = derive_stability(&bulk(), None);
            assert_eq!(s, Stability::Frozen);
            assert!(sig.contains("bulk_download"));
        }

        #[test]
        fn huggingface_is_frozen() {
            let (s, _) = derive_stability(&hf(), None);
            assert_eq!(s, Stability::Frozen);
        }

        #[test]
        fn http_api_is_versioned() {
            let (s, _) = derive_stability(&http_api(), None);
            assert_eq!(s, Stability::Versioned);
        }

        #[test]
        fn local_file_is_versioned() {
            let (s, _) = derive_stability(&local(), None);
            assert_eq!(s, Stability::Versioned);
        }
    }
}
