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

use serde::{Deserialize, Serialize};

/// Articulation axis — what kind of epistemic content the atom holds.
///
/// `Inventory` and `Argument` and `Trace` are not mutually exclusive
/// (an atom can partake in multiple — see the spec's "humble taxonomy"
/// move). When we need to talk about a single label we use the
/// *dominant* axis of an [`ArticulationVector`]; when we need the
/// full distribution we carry the vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Articulation {
    /// Structural map. "X exists in the corpus" — names, relations,
    /// structure, no claim about meaning. Wikipedia article titles,
    /// catalog entries, vault file frontmatter, code symbol graphs.
    Inventory,
    /// Articulated claim. "The author asserts X" — argued, defined,
    /// taken-up content. SEP claims, design-doc assertions, judicial
    /// reasoning, essay arguments, reference cards with defining
    /// quotes.
    Argument,
    /// Lived activity, time-keyed. "X happened / was said at T" —
    /// performance, not codification. Journal entries, conversation
    /// turns, codex sessions, commits, newsworthy event descriptions.
    Trace,
}

impl Articulation {
    pub const ALL: [Articulation; 3] =
        [Articulation::Inventory, Articulation::Argument, Articulation::Trace];

    /// Lowercase string form for metadata keys / prompt headers.
    pub fn as_str(&self) -> &'static str {
        match self {
            Articulation::Inventory => "inventory",
            Articulation::Argument => "argument",
            Articulation::Trace => "trace",
        }
    }
}

/// Stability axis — what temporal contract the corpus carries.
///
/// Derived per-corpus at install time. See
/// [`derive_stability`][crate::stream_axes::derive_stability] in
/// Stage 2 for the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stability {
    /// Snapshot release; re-ingest replaces wholesale. Wiki Core
    /// snapshot, SEP bulk, Gutenberg works, HuggingFace dataset
    /// dumps.
    Frozen,
    /// Active revision; expected to delta-ingest. Wiki Full
    /// expansions, U.S. Code (annual revisions), Federal Register,
    /// code corpora, watched folders.
    Versioned,
    /// Continuously updated within a window. Wiki Newsworthy,
    /// conversation history, codex telemetry.
    Rolling,
}

impl Stability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stability::Frozen => "frozen",
            Stability::Versioned => "versioned",
            Stability::Rolling => "rolling",
        }
    }
}

/// Multi-membership articulation distribution for a single atom.
///
/// Components are intended to sum to 1.0; the classifier produces
/// vectors that satisfy this by construction. Use [`ArticulationVector::new`]
/// to enforce normalisation when composing manually.
///
/// Why multi-membership: Wittgensteinian family-resemblance is real
/// across the corpora we ingest. A wiki article that opens with a
/// definitional sentence is structurally both Inventory (the article
/// *names* the thing) and Argument (the lead sentence *asserts* what
/// the thing is). Forcing single-classification is a known
/// falsification we used to accept to make systems work; the spec
/// explicitly walks away from it. Retrieval picks a *dominant* axis
/// for sectioning, but the vector is preserved so future Moves can
/// surface secondary memberships ("also-X" hints).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArticulationVector {
    pub inventory: f32,
    pub argument: f32,
    pub trace: f32,
}

impl ArticulationVector {
    /// Construct + normalise. Negative inputs are clamped to 0.
    pub fn new(inventory: f32, argument: f32, trace: f32) -> Self {
        let i = inventory.max(0.0);
        let a = argument.max(0.0);
        let t = trace.max(0.0);
        let sum = i + a + t;
        if sum <= f32::EPSILON {
            return Self::balanced();
        }
        Self {
            inventory: i / sum,
            argument: a / sum,
            trace: t / sum,
        }
    }

    /// Uniform 1/3 across all three axes — the explicit ambiguous
    /// shape. Anchors with this vector are surfaced for review (in
    /// the meta-atlas builder's anomaly bucket).
    pub fn balanced() -> Self {
        let third = 1.0 / 3.0;
        Self { inventory: third, argument: third, trace: third }
    }

    /// Dominant axis = the component with the largest weight. Ties
    /// resolved Inventory → Argument → Trace (alphabetical-arbitrary
    /// but stable across builds).
    pub fn dominant(&self) -> Articulation {
        let mut winner = Articulation::Inventory;
        let mut best = self.inventory;
        if self.argument > best {
            best = self.argument;
            winner = Articulation::Argument;
        }
        if self.trace > best {
            winner = Articulation::Trace;
        }
        winner
    }

    /// Weight on a given axis.
    pub fn weight(&self, axis: Articulation) -> f32 {
        match axis {
            Articulation::Inventory => self.inventory,
            Articulation::Argument => self.argument,
            Articulation::Trace => self.trace,
        }
    }

    /// True when no single axis dominates — every component within
    /// `epsilon` of every other. Anchors that classify as ambiguous
    /// are flagged for Move 6's LLM fallback / Layer 3 review.
    pub fn is_ambiguous(&self, epsilon: f32) -> bool {
        let max = self.inventory.max(self.argument).max(self.trace);
        let min = self.inventory.min(self.argument).min(self.trace);
        (max - min) <= epsilon
    }

    /// True when the dominant axis exceeds `threshold`. Retrieval
    /// uses this to decide whether an anchor's dominance is strong
    /// enough to claim a prompt slot — under-threshold anchors are
    /// suppressed.
    pub fn dominant_above(&self, threshold: f32) -> bool {
        self.weight(self.dominant()) >= threshold
    }
}

impl Default for ArticulationVector {
    fn default() -> Self {
        Self::balanced()
    }
}

/// Persisted combined shape — written into `_corpus_meta.json` at
/// install time. The corpus's stream block; per-corpus stability
/// only. Articulation lives on each atom's anchor in the meta-atlas
/// (not here), so the per-corpus shape is just `{stability, source,
/// derived_at, from_signal}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamAxes {
    pub stability: Stability,
    /// Where the stability value came from: `"derived"` (from recipe
    /// fields), `"recipe_override"` (`[corpus.stream] stability = ...`
    /// in the recipe TOML), or `"backfill"` (Stage 2's
    /// `sovereign corpus stream-axes` filled in a legacy meta file).
    pub source: StreamAxesSource,
    /// Unix seconds at which the block was written. Matches the
    /// timestamp shape `IndexMeta::created_at` / `last_updated` use,
    /// so an operator can `date -r <derived_at>` to read it.
    pub derived_at: u64,
    /// Free-text summary of the recipe signal that drove the
    /// derivation. Empty when source is `recipe_override`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_signal: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamAxesSource {
    Derived,
    RecipeOverride,
    Backfill,
}

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
        AcquirerConfig::BulkDownload { .. } => {
            (Stability::Frozen, "acquire=bulk_download")
        }
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
pub fn timestamp_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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
            return (
                Stability::Rolling,
                format!("parent_corpus_id={parent}"),
            );
        }
    }
    if info.corpus_id.starts_with("conversation")
        || info.corpus_id.contains("history")
        || info.corpus_id.contains("codex-session")
    {
        return (
            Stability::Rolling,
            format!("corpus_id={}", info.corpus_id),
        );
    }
    // Watched-folder corpora: ids stamped by the watcher tooling
    // (`watched-<hex>`, `folder-<hex>`). Live local-file content; the
    // user keeps editing files. Versioned by definition.
    if info.corpus_id.starts_with("watched-") || info.corpus_id.starts_with("folder-") {
        return (
            Stability::Versioned,
            format!("corpus_id={}", info.corpus_id),
        );
    }
    match info.kind {
        crate::types::CorpusKind::Catalog => {
            (Stability::Frozen, "kind=catalog".to_string())
        }
        crate::types::CorpusKind::Code => {
            (Stability::Versioned, "kind=code".to_string())
        }
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
    fn articulation_vector_normalises_on_construct() {
        let v = ArticulationVector::new(2.0, 6.0, 2.0);
        assert!((v.inventory - 0.2).abs() < 1e-6);
        assert!((v.argument - 0.6).abs() < 1e-6);
        assert!((v.trace - 0.2).abs() < 1e-6);
    }

    #[test]
    fn balanced_vector_has_uniform_weights() {
        let v = ArticulationVector::balanced();
        assert!((v.inventory - v.argument).abs() < 1e-6);
        assert!((v.argument - v.trace).abs() < 1e-6);
    }

    #[test]
    fn zero_input_falls_back_to_balanced() {
        let v = ArticulationVector::new(0.0, 0.0, 0.0);
        assert_eq!(v, ArticulationVector::balanced());
    }

    #[test]
    fn negative_inputs_clamped_to_zero() {
        let v = ArticulationVector::new(-1.0, 0.5, 0.5);
        assert_eq!(v.inventory, 0.0);
        assert!((v.argument - 0.5).abs() < 1e-6);
        assert!((v.trace - 0.5).abs() < 1e-6);
    }

    #[test]
    fn dominant_picks_largest_axis() {
        let v = ArticulationVector::new(0.1, 0.8, 0.1);
        assert_eq!(v.dominant(), Articulation::Argument);
        let v = ArticulationVector::new(0.7, 0.2, 0.1);
        assert_eq!(v.dominant(), Articulation::Inventory);
        let v = ArticulationVector::new(0.1, 0.1, 0.8);
        assert_eq!(v.dominant(), Articulation::Trace);
    }

    #[test]
    fn ambiguous_flag_fires_on_balanced() {
        let v = ArticulationVector::balanced();
        assert!(v.is_ambiguous(0.05));
    }

    #[test]
    fn ambiguous_flag_clear_on_dominant() {
        let v = ArticulationVector::new(0.7, 0.2, 0.1);
        assert!(!v.is_ambiguous(0.05));
    }

    #[test]
    fn dominant_above_threshold_check() {
        let v = ArticulationVector::new(0.45, 0.30, 0.25);
        assert!(v.dominant_above(0.40));
        assert!(!v.dominant_above(0.50));
    }

    #[test]
    fn axis_as_str_matches_serde_snake_case() {
        assert_eq!(Articulation::Inventory.as_str(), "inventory");
        assert_eq!(Articulation::Argument.as_str(), "argument");
        assert_eq!(Articulation::Trace.as_str(), "trace");
        assert_eq!(Stability::Frozen.as_str(), "frozen");
        assert_eq!(Stability::Versioned.as_str(), "versioned");
        assert_eq!(Stability::Rolling.as_str(), "rolling");
    }

    #[test]
    fn articulation_roundtrips_through_json() {
        let v = ArticulationVector::new(0.6, 0.3, 0.1);
        let s = serde_json::to_string(&v).unwrap();
        let back: ArticulationVector = serde_json::from_str(&s).unwrap();
        assert!((v.inventory - back.inventory).abs() < 1e-6);
        assert!((v.argument - back.argument).abs() < 1e-6);
        assert!((v.trace - back.trace).abs() < 1e-6);
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
            AcquirerConfig::LocalFile { path: "/tmp/x".into() }
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
