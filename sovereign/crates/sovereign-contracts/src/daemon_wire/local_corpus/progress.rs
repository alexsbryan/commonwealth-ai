// SPDX-License-Identifier: AGPL-3.0-or-later
//! Progress events for the local-corpus flows — one enum spanning every
//! phase of both flows, so a surface needs one listener per job.
//!
//! Moved down from `sovereign_tools::local_corpus::progress` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use serde::{Deserialize, Serialize};

use super::pre_scanner::FileMeta;

/// One event on the `local-corpus://progress/{job_id}` channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", content = "data", rename_all = "snake_case")]
pub enum LocalCorpusProgress {
    // Shared phases (both flows) ─────────────────────────────────
    /// Walking the directory tree and classifying files. Emitted by
    /// `PreScanner`.
    Scanning {
        done: usize,
        total: usize,
    },

    /// Staging extracted text out of PDFs/MD into a JSONL file. One
    /// event per file completed.
    Staging {
        done: usize,
        total: usize,
        current_file: String,
    },

    /// One page within a scanned PDF that's currently being OCR'd.
    /// Emitted by the OCR pipeline (`local_corpus::ocr::pipeline`)
    /// before each page hits Tesseract. `file_idx` / `file_total`
    /// count across the OCR queue, so the UI can show "Reading
    /// scanned documents — `<file>`, page N of M (file 3 of 12)".
    OcrPage {
        file: String,
        page: u32,
        total_pages: u32,
        file_idx: u32,
        file_total: u32,
    },

    /// The chunking / embedding / indexing phase — a straight
    /// passthrough of `corpus_engine::progress::IngestProgress`.
    Ingesting {
        done: u64,
        total: u64,
        phase_label: String,
        current_file: Option<String>,
    },

    // Obsidian-only phases ──────────────────────────────────────
    /// Clustering + labeling + open-question detection. M4.
    Clustering {
        stage: ClusterStage,
    },

    /// Writing a snapshot before touching any note. M5.
    Snapshotting {
        done: usize,
        total: usize,
    },

    /// Writing `sovereign/*` tags into note frontmatter. M5.
    Writing {
        done: usize,
        total: usize,
    },

    /// Restoring from a snapshot. M5.
    RollingBack {
        done: usize,
        total: usize,
    },

    // Terminal phases ───────────────────────────────────────────
    Complete {
        result: CompletionResult,
    },

    Error {
        message: String,
        recoverable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Copy)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum ClusterStage {
    EmbeddingMatrix,
    HdbscanRun,
    LlmLabeling,
    OpenQuestionDetection,
}

/// Terminal payload attached to a `Complete` event. Untagged because
/// the manager knows which operation it ran; the variant carries the
/// shape the UI needs for that screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompletionResult {
    Ingest(super::manager::IngestStats),
    // Placeholders for later milestones — the concrete types land with
    // M5 (`WriteBackResult`, `RollbackResult`, `CleanResult`).
}

/// One excerpt chunk selected for the completion screen (spec §5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcerptChunk {
    pub text: String,
    pub source_name: String,
    pub page_ref: Option<String>,
}

// ─── Staging helpers ──────────────────────────────────────────────────

/// A runtime failure during ingestion: a file that pre-scan approved
/// but that actually errored when extracted. Named individually at the
/// completion screen per spec §9.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeFailure {
    pub file: FileMeta,
    pub reason: String,
}
