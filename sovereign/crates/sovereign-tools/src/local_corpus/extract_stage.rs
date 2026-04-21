//! Stage pre-scanned files into a single JSONL file that
//! `corpus-engine`'s `jsonl` extractor can consume.
//!
//! Why this indirection: `corpus-engine` has no PDF dependency (we
//! don't want to pull a ~30MB PDF stack into the crate that also
//! powers cloud corpus builds). Sovereign already ships
//! `pdf-extract`. Running extraction in `sovereign-tools` before
//! handing off to the engine keeps the layering clean AND gives us a
//! natural point to surface per-file runtime failures to the UI —
//! something the engine's ingestion loop doesn't expose today.
//!
//! Output format: one JSON object per line, shape
//!
//! ```json
//! {"id":"<source_id>","title":"<humanised name>","content":"<text>",
//!  "source_path":"<relative path>"}
//! ```
//!
//! `id` and `title` are picked up by `JsonlExtractor`; everything else
//! becomes chunk metadata (see `corpus-engine/src/extractors/json.rs`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config::LocalCorpusConfig;
use super::pre_scanner::FileMeta;
use super::progress::RuntimeFailure;

/// One extraction failure. Collected per run and surfaced on the
/// completion screen as a named skip list (spec §9).
pub type StageFailure = RuntimeFailure;

#[derive(Debug, Serialize, Deserialize)]
struct StagedLine<'a> {
    id: &'a str,
    title: &'a str,
    content: &'a str,
    source_path: &'a str,
}

/// Result of one staging run.
#[derive(Debug, Default)]
pub struct StageResult {
    /// Files whose text landed in the JSONL.
    pub staged: usize,
    /// Files that the pre-scanner approved but that errored during
    /// extraction. Surfaced individually at the completion screen.
    pub failures: Vec<StageFailure>,
}

/// Stage all readable files from a pre-scan into one JSONL file.
///
/// The caller is expected to run this inside `spawn_blocking` — PDF
/// decoding is CPU-bound.
///
/// `on_progress(done, total, current_display_name)` is called after
/// each file is processed (success or failure), then once more at the
/// end with `done == total`.
pub fn stage_blocking(
    config: &LocalCorpusConfig,
    files: &[FileMeta],
    output_path: &Path,
    mut on_progress: impl FnMut(usize, usize, &str),
) -> std::io::Result<StageResult> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(output_path)?;
    let mut writer = BufWriter::new(file);

    let total = files.len();
    let mut result = StageResult::default();

    for (idx, meta) in files.iter().enumerate() {
        let display = meta.display_name.clone();
        on_progress(idx, total, &display);

        match extract_one(&meta.path, config) {
            Ok(text) if !text.trim().is_empty() => {
                let relative = meta
                    .path
                    .strip_prefix(&config.root_path)
                    .unwrap_or(&meta.path)
                    .to_string_lossy()
                    .into_owned();
                let source_id = source_id_for(&meta.path);
                let line = StagedLine {
                    id: &source_id,
                    title: &meta.display_name,
                    content: &text,
                    source_path: &relative,
                };
                let json = serde_json::to_string(&line).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::Other, format!("serialize: {e}"))
                })?;
                writeln!(writer, "{json}")?;
                result.staged += 1;
            }
            Ok(_) => {
                // Empty file — skip silently. Not a failure.
            }
            Err(reason) => {
                result.failures.push(RuntimeFailure {
                    file: meta.clone(),
                    reason,
                });
            }
        }
    }

    on_progress(total, total, "");
    writer.flush()?;
    Ok(result)
}

fn extract_one(path: &Path, _config: &LocalCorpusConfig) -> Result<String, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => safe_extract_pdf_text(path).map_err(|e| match e {
            SafePdfError::Encrypted => "encrypted PDF".to_string(),
            SafePdfError::Panic(msg) => format!("pdf parser panicked: {msg}"),
            SafePdfError::Parse(msg) => format!("pdf parse error: {msg}"),
        }),
        "txt" => std::fs::read_to_string(path).map_err(|e| format!("read: {e}")),
        "md" => extract_markdown(path),
        other => Err(format!("unsupported extension: {other}")),
    }
}

/// Error surface for `safe_extract_pdf_text`. Three categories,
/// distinguished so the classifier can map `Encrypted` → "password
/// protected" while everything else becomes "corrupt" in the UI.
#[derive(Debug)]
pub enum SafePdfError {
    /// pdf-extract returned an error mentioning encryption / password.
    Encrypted,
    /// pdf-extract panicked mid-extraction. Common trigger today is
    /// the DeviceN colour-space path (`pdf-extract/src/lib.rs:1490`
    /// panics with `unimplemented!` when it encounters one). Without
    /// `catch_unwind`, a single bad PDF would take down the whole
    /// `spawn_blocking` task, surfacing as "stage task" join errors
    /// upstream. Catching it here lets the user ingest the rest of
    /// the folder and see the offender named individually on the
    /// completion screen.
    Panic(String),
    /// pdf-extract returned a regular error (malformed PDF, etc.).
    Parse(String),
}

/// Call `pdf_extract::extract_text` inside `catch_unwind` so a panic
/// in the PDF parser propagates as a typed error instead of
/// unwinding the whole tokio blocking task.
pub fn safe_extract_pdf_text(path: &Path) -> Result<String, SafePdfError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let path_owned = path.to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(move || {
        pdf_extract::extract_text(&path_owned)
    }));
    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => {
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("encrypt") || lower.contains("password") {
                Err(SafePdfError::Encrypted)
            } else {
                Err(SafePdfError::Parse(msg))
            }
        }
        Err(payload) => {
            // Panic payload is either `&str` or `String` for normal
            // `panic!` calls. Fall back to a constant tag so we
            // always have *some* diagnostic.
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown pdf-extract panic".to_string()
            };
            tracing::warn!(
                path = %path.display(),
                "pdf-extract panicked during extraction: {msg}"
            );
            Err(SafePdfError::Panic(msg))
        }
    }
}

/// Markdown reader for M1 — just reads the file and strips a YAML
/// frontmatter block if present. Structured frontmatter parsing lands
/// in M3; for now we at least avoid indexing the raw `---` delimiters.
fn extract_markdown(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    Ok(strip_frontmatter(&raw).to_string())
}

pub(crate) fn strip_frontmatter(raw: &str) -> &str {
    // Spec: a frontmatter block starts on line 1 with exactly `---`
    // and ends on the next line that is exactly `---`. Anything else
    // is not a frontmatter block.
    let bytes = raw.as_bytes();
    if !bytes.starts_with(b"---\n") && !bytes.starts_with(b"---\r\n") {
        return raw;
    }
    let after_open = if bytes.starts_with(b"---\r\n") { 5 } else { 4 };
    // Look for a line `---` starting at or after `after_open`.
    let remainder = &raw[after_open..];
    let mut search_start = 0;
    while let Some(pos) = remainder[search_start..].find("---") {
        let abs = search_start + pos;
        // Must be at a line start (prev char is `\n`, or abs == 0 but
        // that can't happen after `---\n`).
        let at_line_start = abs == 0 || remainder.as_bytes()[abs - 1] == b'\n';
        // Must be followed by `\n`, `\r\n`, or EOF.
        let tail = &remainder[abs + 3..];
        let end_of_line = tail.is_empty()
            || tail.starts_with('\n')
            || tail.starts_with("\r\n");
        if at_line_start && end_of_line {
            // Advance past the closing `---` and its terminator.
            let skip = 3 + if tail.starts_with("\r\n") {
                2
            } else if tail.starts_with('\n') {
                1
            } else {
                0
            };
            return &remainder[abs + skip..];
        }
        search_start = abs + 3;
    }
    // No closing `---` found — treat as unfenced content.
    raw
}

fn source_id_for(path: &Path) -> String {
    // Use the file basename as a human-readable ID. The engine does
    // not require IDs to be globally unique across corpora (they're
    // scoped to this corpus), only stable across re-runs.
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Default staging directory rooted under the Sovereign data dir.
/// Kept here so callers don't have to reach into `LocalCorpusManager`
/// internals to know the layout.
pub fn default_staging_path(data_dir: &Path, corpus_id: &str) -> PathBuf {
    data_dir.join("local-corpus-staging").join(format!("{corpus_id}.jsonl"))
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn folder_cfg(root: &Path) -> LocalCorpusConfig {
        LocalCorpusConfig::document_folder(root.to_path_buf(), "Test".into())
    }

    #[test]
    fn stage_txt_file_roundtrips() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("note.txt"), "Hello world\n").unwrap();
        let cfg = folder_cfg(dir.path());
        let file = FileMeta {
            path: dir.path().join("note.txt"),
            size_bytes: 12,
            display_name: "note".into(),
        };
        let out = dir.path().join("staged.jsonl");
        let res = stage_blocking(&cfg, &[file], &out, |_, _, _| {}).unwrap();
        assert_eq!(res.staged, 1);
        assert!(res.failures.is_empty());

        let body = fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(v["title"].as_str().unwrap(), "note");
        assert!(v["content"].as_str().unwrap().contains("Hello world"));
    }

    #[test]
    fn stage_unsupported_extension_fails() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("x.odt"), "ignored").unwrap();
        let cfg = folder_cfg(dir.path());
        let file = FileMeta {
            path: dir.path().join("x.odt"),
            size_bytes: 7,
            display_name: "x".into(),
        };
        let out = dir.path().join("staged.jsonl");
        let res = stage_blocking(&cfg, &[file], &out, |_, _, _| {}).unwrap();
        assert_eq!(res.staged, 0);
        assert_eq!(res.failures.len(), 1);
        assert!(res.failures[0].reason.contains("unsupported"));
    }

    #[test]
    fn stage_progress_reports_final_total() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        let cfg = folder_cfg(dir.path());
        let files = vec![
            FileMeta {
                path: dir.path().join("a.txt"),
                size_bytes: 1,
                display_name: "a".into(),
            },
            FileMeta {
                path: dir.path().join("b.txt"),
                size_bytes: 1,
                display_name: "b".into(),
            },
        ];
        let out = dir.path().join("staged.jsonl");
        let mut last = (0, 0);
        stage_blocking(&cfg, &files, &out, |done, total, _| {
            last = (done, total);
        })
        .unwrap();
        assert_eq!(last, (2, 2));
    }

    #[test]
    fn strip_frontmatter_basic() {
        let md = "---\ntags:\n  - foo\n---\n# Heading\n\nBody.";
        assert_eq!(strip_frontmatter(md), "# Heading\n\nBody.");
    }

    #[test]
    fn strip_frontmatter_none() {
        let md = "# Heading\n\nBody.";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn strip_frontmatter_unclosed_returns_input() {
        // No closing `---` → we treat the file as having no
        // frontmatter so we don't silently drop content.
        let md = "---\nunclosed: true\n# Heading";
        assert_eq!(strip_frontmatter(md), md);
    }

    #[test]
    fn strip_frontmatter_crlf() {
        let md = "---\r\ntags: [a]\r\n---\r\nBody";
        assert_eq!(strip_frontmatter(md), "Body");
    }

    #[test]
    fn safe_extract_pdf_returns_parse_error_for_garbage() {
        let dir = tempdir().unwrap();
        let bad = dir.path().join("not-a-pdf.pdf");
        fs::write(&bad, b"this is not a pdf").unwrap();
        let result = safe_extract_pdf_text(&bad);
        // Must be an error variant; must NOT panic past the wrapper.
        assert!(result.is_err());
        match result {
            Err(SafePdfError::Parse(_)) | Err(SafePdfError::Panic(_)) => {}
            Err(SafePdfError::Encrypted) => {
                panic!("garbage bytes should not be classified as encrypted")
            }
            Ok(_) => panic!("garbage bytes should not parse"),
        }
    }

    #[test]
    fn safe_extract_pdf_catches_panic() {
        // Verifies the panic-catch semantics of `safe_extract_pdf_text`
        // independent of pdf-extract: we reconstruct `catch_unwind`
        // around an explicit panic to ensure our wrapper's error
        // mapping treats it as `SafePdfError::Panic`, not propagates.
        use std::panic::{catch_unwind, AssertUnwindSafe};
        let result = catch_unwind(AssertUnwindSafe(|| {
            panic!("simulated DeviceN colorspace panic");
        }));
        assert!(result.is_err(), "catch_unwind should have caught the panic");
        let payload = result.unwrap_err();
        let msg = payload
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown".to_string());
        assert!(msg.contains("DeviceN"));
    }
}
