// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a pre-scan of a folder or vault FOUND — the verdict a user is
//! shown before any ingest starts.
//!
//! Moved down from `sovereign_tools::local_corpus::pre_scanner` at svt-6
//! (2026-09-12) and re-exported there at the historical path. Pure serde over
//! primitives: a client that only wants to SPELL one of these had to link
//! `sovereign-tools` — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more. See this module's parent for the full note.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub display_name: String,
}

// `FileMeta::from_path` stayed in `sovereign_tools::local_corpus::pre_scanner`
// as the free function `file_meta_from_path` (svt-6). It humanises the
// filename through `humanise::humanise_display_name` — spec §5.3's rules,
// which are behaviour and not schema. An inherent impl has to live in the
// crate that defines the type, so the constructor could not follow the struct
// down; nothing outside this workspace's walker constructs a `FileMeta`.

/// What kind of PDF a file is, for the purposes of pre-scan. The
/// classifier is approximate — it runs a fast text-density heuristic
/// on the first pages rather than a true OCR-readiness probe.
///
/// `ScannedNoText` is the OCR-eligible bucket. It covers two cases the
/// UI treats identically: (a) PDFs with a text layer that's empty
/// (true scanned-image PDFs), and (b) PDFs that pdf-extract panicked
/// or errored on but PDFium can probably still rasterize. Lumping the
/// two means a user with one "weird" PDF that pdf-extract chokes on
/// (e.g. DeviceN colourspace, non-standard font tables) still gets the
/// OCR offer instead of seeing a flat "couldn't be read" message with
/// no recovery path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfClass {
    Readable,
    ScannedNoText,
    PasswordProtected,
    Corrupt,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreScanResult {
    /// Files that will be indexed.
    pub readable: Vec<FileMeta>,
    /// Scanned PDFs (no text layer). Named individually in the UI.
    pub scanned_pdfs: Vec<FileMeta>,
    /// Password-protected PDFs. Named individually.
    pub protected_pdfs: Vec<FileMeta>,
    /// Corrupt / unparseable files. Named individually.
    pub corrupt_files: Vec<FileMeta>,
    /// Files larger than `large_file_threshold_mb`. Still indexed, but
    /// the UI surfaces them as slow.
    pub large_files: Vec<FileMeta>,
    /// Count of files whose extension was outside the allow-list.
    /// NOT named — per §9, "unsupported types" is a count-only skip.
    pub ignored_types: u32,
    /// Per-extension breakdown of `ignored_types`. Lower-case
    /// extension (without the leading dot) → count. The watched-folder
    /// status surface uses this so a user who drops 200 `.docx` files
    /// gets a visible answer to "why isn't this searchable?" rather
    /// than seeing only the aggregate `ignored_types` number. Empty
    /// for the existing DropFolder + ObsidianVault flows that don't
    /// surface the breakdown — there's no compatibility risk because
    /// `#[serde(default)]` lets older sidecars deserialize cleanly.
    #[serde(default)]
    pub skipped_by_extension: std::collections::HashMap<String, usize>,
    /// Total files visited (informational).
    pub total_visited: u32,
}

impl PreScanResult {
    /// Count of files the user expected to see indexed but that will
    /// be skipped for a reason they'd probably want to know about.
    pub fn named_skip_count(&self) -> usize {
        self.scanned_pdfs.len() + self.protected_pdfs.len() + self.corrupt_files.len()
    }
}
