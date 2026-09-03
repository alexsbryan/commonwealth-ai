// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! `/Users/user/.claude/plans/binary-scribbling-babbage.md`.

pub mod atlas_dispatch;
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
pub mod recipe_extractor;
pub mod watched;
pub mod writeback;

pub use config::{
    CorpusScope, DeletionGuardConfig, LocalCorpusConfig, LocalCorpusSourceType, PreScanConfig,
    WatchedFolderConfig, WatcherConfig, WriteBackConfig,
};
pub use humanise::humanise_display_name;
pub use manager::{IncompleteJob, IngestStats, LocalCorpusManager, WatchedIncompleteJob};
pub use pre_scanner::{FileMeta, PdfClass, PreScanResult, PreScanner};
pub use progress::{ClusterStage, CompletionResult, ExcerptChunk, LocalCorpusProgress};
pub use watched::{
    Scheduler as WatchedFolderScheduler, WatchedFolderEvent, WatchedFolderRegistry,
    WatchedFolderStatus, Worker as WatchedFolderWorker,
};
