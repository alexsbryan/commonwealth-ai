//! Pre-scan: classify every file in a local corpus BEFORE ingestion.
//!
//! Produces the `PreScanResult` the UI renders on the confirmation
//! screen. Named categories (scanned PDFs, password-protected PDFs,
//! corrupt files) list individual filenames — the user needs to know
//! which documents won't be indexed and why. Unsupported file types are
//! counted only, not named (spec §9).
//!
//! Walker contract:
//!   - Hidden files and hidden directories (`.obsidian/`, `.git/`) are
//!     silently skipped, not counted.
//!   - Extensions outside the config's allow-list are counted in
//!     `ignored_types` and not named.
//!   - Files above `large_file_threshold_mb` are flagged as slow but
//!     still indexed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use super::config::LocalCorpusConfig;
use super::humanise::humanise_display_name;

// ─── File metadata ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub display_name: String,
}

impl FileMeta {
    fn from_path(path: PathBuf, size_bytes: u64) -> Self {
        let display_name = humanise_display_name(&path);
        Self {
            path,
            size_bytes,
            display_name,
        }
    }
}

// ─── PDF classification ───────────────────────────────────────────────

/// What kind of PDF a file is, for the purposes of pre-scan. The
/// classifier is approximate — it runs a fast text-density heuristic
/// on the first pages rather than a true OCR-readiness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfClass {
    Readable,
    ScannedNoText,
    PasswordProtected,
    Corrupt,
}

/// Classify a single PDF. Blocking — intended to run inside
/// `tokio::task::spawn_blocking`.
///
/// Heuristic (spec §5.1):
///   1. Try to extract text. A pdf-extract error whose message
///      mentions encryption means `PasswordProtected`. Other errors
///      mean `Corrupt`.
///   2. Count words in the extracted text's first 4000 characters
///      (≈ first two pages at standard density).
///   3. If `size_kb > 100 && word_count < 20` → `ScannedNoText`.
///   4. Otherwise → `Readable`.
pub fn classify_pdf_blocking(path: &Path) -> PdfClass {
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    match crate::local_corpus::extract_stage::safe_extract_pdf_text(path) {
        Err(SafePdfError::Encrypted) => PdfClass::PasswordProtected,
        Err(SafePdfError::Panic(_)) | Err(SafePdfError::Parse(_)) => PdfClass::Corrupt,
        Ok(text) => {
            let size_kb = size_bytes / 1024;
            // Look at first ~4KB of extracted text (rough proxy for
            // "first two pages"). Splitting on Unicode whitespace is a
            // good-enough word count for the heuristic.
            let head: String = text.chars().take(4000).collect();
            let word_count = head.split_whitespace().count();
            if size_kb > 100 && word_count < 20 {
                PdfClass::ScannedNoText
            } else {
                PdfClass::Readable
            }
        }
    }
}

pub use crate::local_corpus::extract_stage::SafePdfError;

// ─── Pre-scan result ─────────────────────────────────────────────────

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

// ─── Pre-scanner ─────────────────────────────────────────────────────

pub struct PreScanner<'a> {
    config: &'a LocalCorpusConfig,
}

impl<'a> PreScanner<'a> {
    pub fn new(config: &'a LocalCorpusConfig) -> Self {
        Self { config }
    }

    /// Run a synchronous pre-scan. CPU-bound (opens PDFs); callers in
    /// async contexts should wrap in `spawn_blocking`. `on_progress` is
    /// called with `(scanned_so_far, total_estimate)` — the total is
    /// the count of files matching the extension filter, known after
    /// the first pass. Before that, `total_estimate` is the count of
    /// files-so-far.
    pub fn run_blocking(
        &self,
        mut on_progress: impl FnMut(usize, usize),
    ) -> PreScanResult {
        // First pass: collect candidate paths + sizes and count files
        // whose extension didn't match the allow-list.
        let (candidates, ignored_types, total_visited) = self.collect_candidates();
        let total = candidates.len();

        let mut result = PreScanResult {
            total_visited,
            ignored_types,
            ..Default::default()
        };

        let threshold_bytes = self.config.pre_scan.large_file_threshold_mb * 1024 * 1024;

        for (idx, (path, size_bytes)) in candidates.into_iter().enumerate() {
            on_progress(idx, total);

            let meta = FileMeta::from_path(path.clone(), size_bytes);
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();

            let is_pdf = ext == "pdf";

            if is_pdf && self.config.pre_scan.password_detection {
                match classify_pdf_blocking(&path) {
                    PdfClass::PasswordProtected => {
                        result.protected_pdfs.push(meta);
                        continue;
                    }
                    PdfClass::Corrupt => {
                        result.corrupt_files.push(meta);
                        continue;
                    }
                    PdfClass::ScannedNoText if self.config.pre_scan.scanned_pdf_detection => {
                        result.scanned_pdfs.push(meta);
                        continue;
                    }
                    // Readable, or Scanned but detection disabled — fall
                    // through into the readable bucket.
                    _ => {}
                }
            }

            if threshold_bytes > 0 && size_bytes > threshold_bytes {
                result.large_files.push(meta.clone());
            }
            result.readable.push(meta);
        }
        on_progress(total, total);
        result
    }

    /// Returns `(candidates, ignored_type_count, total_files_visited)`.
    fn collect_candidates(&self) -> (Vec<(PathBuf, u64)>, u32, u32) {
        let allowed: Vec<String> = self
            .config
            .extensions
            .iter()
            .map(|e| e.to_ascii_lowercase())
            .collect();

        let mut candidates = Vec::new();
        let mut ignored_extensions: u32 = 0;
        let mut total_visited: u32 = 0;

        // filter_entry runs on the walker root too — if the user's
        // chosen folder happens to be hidden (e.g. a macOS tempdir
        // named `.tmpXYZ`), we still want to descend into it. Skip
        // hidden-file logic when depth == 0.
        for entry in WalkDir::new(&self.config.root_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e.path()))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            total_visited = total_visited.saturating_add(1);
            let path = entry.path().to_path_buf();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase());
            match ext {
                Some(ext) if allowed.iter().any(|a| a == &ext) => {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    candidates.push((path, size));
                }
                _ => {
                    ignored_extensions = ignored_extensions.saturating_add(1);
                }
            }
        }

        (candidates, ignored_extensions, total_visited)
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.') && n != "." && n != "..")
        .unwrap_or(false)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_folder_config(path: &Path) -> LocalCorpusConfig {
        LocalCorpusConfig::document_folder(path.to_path_buf(), "Test folder".into())
    }

    #[test]
    fn walks_and_filters_by_extension() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        fs::write(dir.path().join("b.md"), b"world").unwrap(); // not in allow-list
        fs::write(dir.path().join("c.pdf"), b"not a real pdf").unwrap();

        let cfg = make_folder_config(dir.path());
        let scanner = PreScanner::new(&cfg);
        let result = scanner.run_blocking(|_, _| {});

        // `.md` is outside the allow-list for DocumentFolder — counted
        // as ignored. `.pdf` is in the list but it's corrupt (not a
        // real PDF). `.txt` is readable.
        assert_eq!(result.readable.len(), 1);
        assert_eq!(result.readable[0].display_name, "a");
        // The non-real PDF lands in `corrupt_files`.
        assert_eq!(result.corrupt_files.len(), 1);
        // `.md` doesn't match the DocumentFolder allow-list.
        assert_eq!(result.ignored_types, 1);
    }

    #[test]
    fn hidden_files_and_dirs_skipped() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("visible.txt"), b"hi").unwrap();
        fs::write(dir.path().join(".secret.txt"), b"hi").unwrap();
        let hidden_dir = dir.path().join(".git");
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("inside.txt"), b"hi").unwrap();

        let cfg = make_folder_config(dir.path());
        let scanner = PreScanner::new(&cfg);
        let result = scanner.run_blocking(|_, _| {});

        // Only `visible.txt` should survive.
        assert_eq!(result.readable.len(), 1);
        assert_eq!(result.readable[0].display_name, "visible");
    }

    #[test]
    fn nested_subdirs_traversed() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub/deep");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("top.txt"), b"1").unwrap();
        fs::write(sub.join("buried.txt"), b"2").unwrap();

        let cfg = make_folder_config(dir.path());
        let scanner = PreScanner::new(&cfg);
        let result = scanner.run_blocking(|_, _| {});

        assert_eq!(result.readable.len(), 2);
    }

    #[test]
    fn large_files_flagged_but_still_readable() {
        let dir = tempdir().unwrap();
        // Write a 1MB text file.
        let big = vec![b'x'; 1024 * 1024];
        fs::write(dir.path().join("big.txt"), &big).unwrap();

        let mut cfg = make_folder_config(dir.path());
        // Lower the threshold to 0 MB so any non-empty file is "large".
        cfg.pre_scan.large_file_threshold_mb = 0;
        // Threshold of 0 → `threshold_bytes = 0`, which we interpret as
        // "flagging disabled" in the current impl. Override via a
        // positive byte threshold via a slightly lower number.
        //
        // Per current impl, `threshold_bytes == 0` disables flagging,
        // matching the intent that "0" means "off". For this test we
        // want flagging on, so set a low positive threshold.
        cfg.pre_scan.large_file_threshold_mb = 1; // threshold = 1 MB
        // Our file is exactly 1 MB; > 1 MB would be 1,048,577 bytes.
        // Bump threshold logic: test with threshold 0.5 MB. `u64` only,
        // so use 1 and add a second file slightly over.
        fs::write(dir.path().join("bigger.txt"), vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();

        let scanner = PreScanner::new(&cfg);
        let result = scanner.run_blocking(|_, _| {});

        assert_eq!(result.readable.len(), 2);
        // `bigger.txt` is > 1MB, `big.txt` is = 1MB.
        assert_eq!(result.large_files.len(), 1);
        assert_eq!(result.large_files[0].display_name, "bigger");
    }

    #[test]
    fn empty_folder_returns_empty_result() {
        let dir = tempdir().unwrap();
        let cfg = make_folder_config(dir.path());
        let scanner = PreScanner::new(&cfg);
        let result = scanner.run_blocking(|_, _| {});
        assert_eq!(result.readable.len(), 0);
        assert_eq!(result.total_visited, 0);
    }

    #[test]
    fn progress_callback_fires() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"a").unwrap();
        fs::write(dir.path().join("b.txt"), b"b").unwrap();

        let cfg = make_folder_config(dir.path());
        let scanner = PreScanner::new(&cfg);
        let mut calls = Vec::new();
        scanner.run_blocking(|done, total| calls.push((done, total)));
        // Final call must report done == total.
        let last = calls.last().copied().unwrap();
        assert_eq!(last.0, last.1);
        assert_eq!(last.0, 2);
    }

    #[test]
    fn pdf_classification_corrupt() {
        let dir = tempdir().unwrap();
        let bad_pdf = dir.path().join("bad.pdf");
        fs::write(&bad_pdf, b"definitely not a pdf file").unwrap();
        let class = classify_pdf_blocking(&bad_pdf);
        // The bytes aren't a valid PDF header — classifier returns
        // either Corrupt or (less likely) PasswordProtected. We accept
        // either; the test guarantees it's not `Readable`.
        assert_ne!(class, PdfClass::Readable);
        assert_ne!(class, PdfClass::ScannedNoText);
    }

    #[test]
    fn named_skip_count_sums_named_categories() {
        let r = PreScanResult {
            scanned_pdfs: vec![dummy_meta("a")],
            protected_pdfs: vec![dummy_meta("b"), dummy_meta("c")],
            corrupt_files: vec![dummy_meta("d")],
            ..Default::default()
        };
        assert_eq!(r.named_skip_count(), 4);
    }

    fn dummy_meta(name: &str) -> FileMeta {
        FileMeta {
            path: PathBuf::from(name),
            size_bytes: 0,
            display_name: name.into(),
        }
    }
}
