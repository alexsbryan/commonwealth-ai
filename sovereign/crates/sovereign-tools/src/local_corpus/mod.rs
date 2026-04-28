//! Local corpus management — shared abstraction over "Obsidian Vault"
//! and "Folder Drop". Both flows are instances of the same operation:
//! the user points Sovereign at a local directory, Sovereign scans +
//! ingests + indexes + (optionally) watches it, and the relationship is
//! maintained over time.
//!
//! Architecture:
//!
//! ```text
//! LocalCorpusManager
//!   ├── PreScanner        — file classification before ingestion
//!   ├── ExtractStage      — PDF/TXT/MD → JSONL staging (per-file failure tolerant)
//!   ├── CorpusEngine      — delegated ingestion (reused from corpus-engine)
//!   ├── VaultWatcher      — filesystem watch + debounced delta (M3)
//!   ├── Clusterer         — HDBSCAN + LLM labeling (Obsidian only, M4)
//!   ├── PreviewBuilder    — assignment + confidence + outliers (Obsidian only, M4)
//!   └── WriteBack         — frontmatter merge + snapshot (Obsidian only, M5)
//! ```
//!
//! Principle: no silent writes. `PreScanner` runs before every ingest;
//! Obsidian write-back is explicit, reversible, and namespaced to
//! `sovereign/*`. See the plan at
//! `/Users/alexsbryan/.claude/plans/binary-scribbling-babbage.md`.

pub mod clusterer;
pub mod config;
pub mod excerpt;
pub mod extract_stage;
pub mod frontmatter;
pub mod git;
pub mod humanise;
pub mod manager;
pub mod ocr;
pub mod pre_scanner;
pub mod preview;
pub mod progress;
pub mod writeback;

pub use config::{
    CorpusScope, LocalCorpusConfig, LocalCorpusSourceType, PreScanConfig, WatcherConfig,
    WriteBackConfig,
};
pub use humanise::humanise_display_name;
pub use manager::{IncompleteJob, IngestStats, LocalCorpusManager};
pub use pre_scanner::{FileMeta, PdfClass, PreScanResult, PreScanner};
pub use progress::{ClusterStage, CompletionResult, ExcerptChunk, LocalCorpusProgress};
