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

/// Classify a single PDF. Blocking — intended to run inside
/// `tokio::task::spawn_blocking`.
///
/// Heuristic (spec §5.1, expanded for OCR-eligibility):
///   1. Try to extract text. A pdf-extract error whose message
///      mentions encryption means `PasswordProtected` (OCR can't help).
///   2. A pdf-extract panic or parse error means `ScannedNoText` —
///      pdf-extract is fragile, but PDFium-backed OCR usually
///      succeeds where it fails.
///   3. Count words in the extracted text's first 4000 characters
///      (≈ first two pages at standard density).
///   4. If `size_kb > 100 && word_count < 20` → `ScannedNoText`.
///   5. Otherwise → `Readable`.
pub fn classify_pdf_blocking(path: &Path) -> PdfClass {
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    match crate::local_corpus::extract_stage::safe_extract_pdf_text(path) {
        Err(SafeExtractError::Encrypted) => PdfClass::PasswordProtected,
        Err(SafeExtractError::Panic(_))
        | Err(SafeExtractError::Parse(_))
        | Err(SafeExtractError::Other(_)) => PdfClass::ScannedNoText,
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

pub use crate::local_corpus::extract_stage::SafeExtractError;

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

// ─── Pre-scanner ─────────────────────────────────────────────────────

pub struct PreScanner<'a> {
    config: &'a LocalCorpusConfig,
    /// Optional override for which path to walk. When `None`, the
    /// scanner walks `config.root_path` (today's behaviour and the
    /// default for single-root corpora). Folder-ingest v1 §3.1
    /// multi-root: the watched-folder walker constructs one
    /// `PreScanner` per root, threading each root in here.
    walk_root: Option<&'a Path>,
}

impl<'a> PreScanner<'a> {
    pub fn new(config: &'a LocalCorpusConfig) -> Self {
        Self {
            config,
            walk_root: None,
        }
    }

    /// Construct a scanner that walks an explicit root path
    /// instead of `config.root_path`. Used by the watched-folder
    /// walker to iterate `WatchedFolderConfig.additional_roots`.
    pub fn with_root(config: &'a LocalCorpusConfig, root: &'a Path) -> Self {
        Self {
            config,
            walk_root: Some(root),
        }
    }

    fn root_path(&self) -> &Path {
        self.walk_root.unwrap_or(&self.config.root_path)
    }

    /// Run a synchronous pre-scan. CPU-bound (opens PDFs); callers in
    /// async contexts should wrap in `spawn_blocking`. `on_progress` is
    /// called with `(scanned_so_far, total_estimate)` — the total is
    /// the count of files matching the extension filter, known after
    /// the first pass. Before that, `total_estimate` is the count of
    /// files-so-far.
    pub fn run_blocking(&self, mut on_progress: impl FnMut(usize, usize)) -> PreScanResult {
        // First pass: collect candidate paths + sizes and count files
        // whose extension didn't match the allow-list.
        let (candidates, ignored_types, total_visited, skipped_by_extension) =
            self.collect_candidates();
        let total = candidates.len();

        let mut result = PreScanResult {
            total_visited,
            ignored_types,
            skipped_by_extension,
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

    /// Returns `(candidates, ignored_type_count, total_files_visited, skipped_by_extension)`.
    fn collect_candidates(
        &self,
    ) -> (
        Vec<(PathBuf, u64)>,
        u32,
        u32,
        std::collections::HashMap<String, usize>,
    ) {
        let allowed: Vec<String> = self
            .config
            .extensions
            .iter()
            .map(|e| e.to_ascii_lowercase())
            .collect();

        // Compile exclude globs once. Source: the source-type's
        // exclude list (today only WatchedFolder carries one;
        // ObsidianVault inherits its excludes via the worker's
        // synthesised WatchedFolderConfig in
        // `worker::reconciliation_config_for`). Pre-fix, only the
        // worker's sweep loop applied these globs and the initial
        // `manager.ingest` skipped them — that's why an obsidian-
        // vault corpus registered with `--exclude COMMONWEALTH/**`
        // would still ingest 179 documents when the walker said 41.
        // Same compile-on-error semantics as walker.rs: a malformed
        // glob warns and is dropped rather than wedging the scan.
        let exclude_globs: Vec<glob::Pattern> = self
            .config
            .source_type
            .watched_config()
            .map(|wf| wf.exclude_globs.clone())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|g| match glob::Pattern::new(&g) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(pattern = %g, "pre_scanner:exclude_glob_invalid: {e}");
                    None
                }
            })
            .collect();
        let walk_root = self.root_path();

        let mut candidates = Vec::new();
        let mut ignored_extensions: u32 = 0;
        let mut total_visited: u32 = 0;
        let mut skipped_by_extension: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        // filter_entry runs on the walker root too — if the user's
        // chosen folder happens to be hidden (e.g. a macOS tempdir
        // named `.tmpXYZ`), we still want to descend into it. Skip
        // hidden-file logic when depth == 0.
        for entry in WalkDir::new(self.root_path())
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

            // Exclude-glob check matches the walker's semantics
            // (walker.rs:274) — patterns evaluated against the path
            // relative to the walk root, forward-slash normalised.
            // A file that any pattern matches is dropped silently
            // here (does NOT bump skipped_by_extension since the
            // skip is by user-rule, not by extension).
            if !exclude_globs.is_empty() {
                if let Ok(rel) = entry.path().strip_prefix(walk_root) {
                    let rel_norm = rel.to_string_lossy().replace('\\', "/");
                    if exclude_globs.iter().any(|p| p.matches(&rel_norm)) {
                        continue;
                    }
                }
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase());
            match ext {
                Some(ext) if allowed.iter().any(|a| a == &ext) => {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    candidates.push((path, size));
                }
                Some(ext) => {
                    // Named-extension skip — bumps both the aggregate
                    // counter (preserved for existing callers) and
                    // the per-extension breakdown the watched-folder
                    // status surface consumes.
                    ignored_extensions = ignored_extensions.saturating_add(1);
                    *skipped_by_extension.entry(ext).or_insert(0) += 1;
                }
                None => {
                    // Extension-less file — count as ignored without
                    // a label. Rare in practice (READMEs etc) but
                    // worth bucketing as `(no extension)` so the UI
                    // total reconciles.
                    ignored_extensions = ignored_extensions.saturating_add(1);
                    *skipped_by_extension
                        .entry("(no extension)".into())
                        .or_insert(0) += 1;
                }
            }
        }

        (
            candidates,
            ignored_extensions,
            total_visited,
            skipped_by_extension,
        )
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
        // as ignored. `.pdf` is in the list but pdf-extract can't parse
        // it (not a real PDF) — that lands it in `scanned_pdfs` so the
        // OCR pipeline gets a chance to recover it. `.txt` is readable.
        assert_eq!(result.readable.len(), 1);
        assert_eq!(result.readable[0].display_name, "a");
        assert_eq!(result.scanned_pdfs.len(), 1);
        assert!(result.corrupt_files.is_empty());
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
        fs::write(
            dir.path().join("bigger.txt"),
            vec![b'x'; 2 * 1024 * 1024 + 1],
        )
        .unwrap();

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
    fn pdf_classification_unparseable_routes_to_ocr_bucket() {
        let dir = tempdir().unwrap();
        let bad_pdf = dir.path().join("bad.pdf");
        fs::write(&bad_pdf, b"definitely not a pdf file").unwrap();
        let class = classify_pdf_blocking(&bad_pdf);
        // pdf-extract is fragile; PDFium-backed OCR usually succeeds
        // where it fails. So non-encrypted extraction failures are
        // routed into the OCR-eligible bucket rather than a flat
        // "couldn't be read" with no recovery.
        assert_eq!(class, PdfClass::ScannedNoText);
    }

    #[test]
    fn pre_scan_honours_watched_folder_exclude_globs() {
        // Regression: a watched-folder corpus registered with
        // `--exclude COMMONWEALTH/**` was still ingesting every file
        // under COMMONWEALTH/. The walker honored the exclude (state
        // showed correct live_docs) but the initial `manager.ingest`
        // path used PreScanner directly and bypassed the rule —
        // resulting in a LanceDB index that contained material the
        // user explicitly asked to skip.
        use super::super::config::{LocalCorpusConfig, LocalCorpusSourceType, WatchedFolderConfig};
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("root.md"), b"keep").unwrap();
        fs::create_dir_all(dir.path().join("COMMONWEALTH")).unwrap();
        fs::write(dir.path().join("COMMONWEALTH/work.md"), b"drop").unwrap();
        fs::create_dir_all(dir.path().join("_sovereign-index")).unwrap();
        fs::write(dir.path().join("_sovereign-index/index.md"), b"drop").unwrap();

        let mut wf = WatchedFolderConfig::default();
        wf.exclude_globs = vec!["COMMONWEALTH/**".into(), "_sovereign-index/**".into()];
        let mut cfg =
            LocalCorpusConfig::watched_folder(dir.path().to_path_buf(), "test".into(), wf.clone());
        // `watched_folder()` factory consumes the WatchedFolderConfig
        // into source_type; force the cfg's extensions to include md
        // (default already does).
        cfg.source_type = LocalCorpusSourceType::WatchedFolder(wf);

        let scanner = PreScanner::new(&cfg);
        let result = scanner.run_blocking(|_, _| {});

        let names: Vec<&str> = result
            .readable
            .iter()
            .map(|m| m.display_name.as_str())
            .collect();
        assert!(
            names.contains(&"root"),
            "root.md must be ingested; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == &"work"),
            "COMMONWEALTH/work.md must be excluded; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == &"index"),
            "_sovereign-index/index.md must be excluded; got {names:?}"
        );
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
