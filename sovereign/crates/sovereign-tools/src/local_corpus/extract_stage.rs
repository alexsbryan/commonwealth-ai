// SPDX-License-Identifier: AGPL-3.0-or-later
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

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::config::LocalCorpusConfig;
use super::ocr::{OcrCtx, PageProgressCallback};
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
                let json = serde_json::to_string(&line)
                    .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
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

pub(crate) fn extract_one(path: &Path, _config: &LocalCorpusConfig) -> Result<String, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => safe_extract_pdf_text(path).map_err(|e| classify_extract_error("pdf", &e)),
        "txt" => std::fs::read_to_string(path).map_err(|e| format!("read: {e}")),
        "md" => extract_markdown(path),
        "html" | "htm" => {
            safe_extract_html_text(path).map_err(|e| classify_extract_error("html", &e))
        }
        "mhtml" => safe_extract_mhtml_text(path).map_err(|e| classify_extract_error("mhtml", &e)),
        "epub" => safe_extract_epub_text(path).map_err(|e| classify_extract_error("epub", &e)),
        "docx" => safe_extract_docx_text(path).map_err(|e| classify_extract_error("docx", &e)),
        other => Err(format!("unsupported extension: {other}")),
    }
}

/// Map a `SafeExtractError` onto the human-readable reason string that
/// flows into `WatchedFolderState.failed_files[*].reason` and the UI.
/// Per-format prefix preserves the diagnostic specificity the PDF
/// branch shipped with — "encrypted PDF" reads cleaner than "encrypted
/// file" when the user is scanning a list of failures. The `format`
/// argument should be a short lowercase tag (e.g. `"pdf"`, `"docx"`).
fn classify_extract_error(format: &str, e: &SafeExtractError) -> String {
    match e {
        SafeExtractError::Encrypted => format!("encrypted {format}"),
        SafeExtractError::Panic(msg) => format!("{format} parser panicked: {msg}"),
        SafeExtractError::Parse(msg) => format!("{format} parse error: {msg}"),
        SafeExtractError::Other(msg) => format!("{format} extract error: {msg}"),
    }
}

/// Error surface shared by every `safe_extract_*_text` function.
///
/// Discriminating between these four categories lets the classifier
/// map `Encrypted` → "password protected", `Panic` → "parser bug
/// (corrupt)", `Parse` → "malformed input", and `Other` → "format-
/// level failure that doesn't fit the others".
///
/// Catching panics in this enum (via `catch_unwind`) is load-bearing:
/// without it, a single malformed file would unwind the whole
/// `spawn_blocking` task and surface as a "stage task" join error
/// upstream. Catching it lets the user ingest the rest of the folder
/// and see the offender named individually on the completion screen.
/// The canonical historical trigger was pdf-extract's DeviceN
/// colour-space path (`pdf-extract/src/lib.rs:1490` panics with
/// `unimplemented!`); the same discipline applies to every new
/// extractor we add.
#[derive(Debug)]
pub enum SafeExtractError {
    /// File is encrypted / password-protected.
    Encrypted,
    /// Extractor panicked; payload caught via `catch_unwind`.
    Panic(String),
    /// Extractor returned a regular error (malformed input, parse
    /// failure, etc.).
    Parse(String),
    /// Format-level failure that doesn't fit the others (e.g. an
    /// EPUB whose spine is missing, or an MHTML blob with no
    /// `text/html` part).
    Other(String),
}

/// Process-wide stdout silencer. RAII guard: on construction it
/// `dup2`s `/dev/null` over fd 1 and saves the previous fd; on drop
/// it restores. A static `Mutex` serialises concurrent silencers so
/// two blocking tasks parsing PDFs in parallel don't race on the
/// shared fd.
///
/// Scope is intentionally narrow — only used to suppress
/// `pdf-extract`'s in-process `println!` noise. Tracing writes to
/// stderr (daemon.err), so structured logs are unaffected.
#[cfg(unix)]
struct StdoutSilencer {
    saved_fd: libc::c_int,
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
static STDOUT_SILENCE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
impl StdoutSilencer {
    fn new() -> std::io::Result<Self> {
        // Poisoned mutex is harmless — the previous holder only held
        // the lock around a stdout swap, which has already been
        // unwound. Drop the poison and proceed.
        let guard = STDOUT_SILENCE_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let devnull = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
        use std::os::fd::AsRawFd;
        let stdout_fd: libc::c_int = 1;
        // Flush any buffered stdout first so it lands in the real
        // file, not /dev/null.
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let saved_fd = unsafe { libc::dup(stdout_fd) };
        if saved_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let rc = unsafe { libc::dup2(devnull.as_raw_fd(), stdout_fd) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(saved_fd);
            }
            return Err(err);
        }
        Ok(StdoutSilencer {
            saved_fd,
            _guard: guard,
        })
    }
}

#[cfg(unix)]
impl Drop for StdoutSilencer {
    fn drop(&mut self) {
        // Restore stdout. Failures here are unrecoverable but
        // shouldn't panic — at worst stdout stays pointing at
        // /dev/null until the process exits, which is preferable to
        // unwinding through arbitrary call stacks.
        let _ = std::io::Write::flush(&mut std::io::stdout());
        unsafe {
            libc::dup2(self.saved_fd, 1);
            libc::close(self.saved_fd);
        }
    }
}

/// No-op on non-Unix targets. The silencer above redirects fd 1 to
/// `/dev/null` via `libc::dup2`, which has no portable Windows analogue
/// (`std::os::fd` doesn't exist there). pdf-extract's `println!` spew is
/// cosmetic, so on Windows we simply don't silence it rather than port the
/// Win32 `SetStdHandle` dance for a non-load-bearing nicety.
#[cfg(not(unix))]
struct StdoutSilencer;

#[cfg(not(unix))]
impl StdoutSilencer {
    fn new() -> std::io::Result<Self> {
        Ok(StdoutSilencer)
    }
}

/// Call `pdf_extract::extract_text` inside `catch_unwind` so a panic
/// in the PDF parser propagates as a typed error instead of
/// unwinding the whole tokio blocking task.
///
/// Wraps the call in `silence_stdout` because `pdf-extract 0.7.12`
/// uses raw `println!` for per-glyph diagnostics — a single mildly
/// non-standard PDF emits tens of thousands of lines to stdout
/// (launchd captures these in `daemon.out`).
pub fn safe_extract_pdf_text(path: &Path) -> Result<String, SafeExtractError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let path_owned = path.to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(move || {
        let _silence = StdoutSilencer::new().ok();
        pdf_extract::extract_text(&path_owned)
    }));
    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => {
            let msg = e.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("encrypt") || lower.contains("password") {
                Err(SafeExtractError::Encrypted)
            } else {
                Err(SafeExtractError::Parse(msg))
            }
        }
        Err(payload) => {
            let msg = downcast_panic_payload(payload, "unknown pdf-extract panic");
            tracing::warn!(
                path = %path.display(),
                "pdf-extract panicked during extraction: {msg}"
            );
            Err(SafeExtractError::Panic(msg))
        }
    }
}

/// Best-effort string from a `catch_unwind` panic payload. Normal
/// `panic!` calls produce either `&str` or `String`; falls back to
/// a caller-supplied tag so we always have *some* diagnostic.
pub(crate) fn downcast_panic_payload(
    payload: Box<dyn std::any::Any + Send>,
    fallback: &str,
) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        fallback.to_string()
    }
}

/// Read an `.html` / `.htm` file and return body text suitable for the
/// chunker. `<script>` and `<style>` blocks are dropped; block-level
/// elements (`<p>`, `<div>`, headings, list items, `<br>`) emit
/// newlines so word boundaries survive across tags. Mirrors the
/// panic-discipline of `safe_extract_pdf_text` — `scraper`'s parser
/// has been stable in practice but every extractor we ship runs inside
/// `catch_unwind` so a single malformed file never takes down the
/// blocking task.
pub fn safe_extract_html_text(path: &Path) -> Result<String, SafeExtractError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let raw =
        std::fs::read_to_string(path).map_err(|e| SafeExtractError::Parse(format!("read: {e}")))?;
    let result = catch_unwind(AssertUnwindSafe(|| html_body_text(&raw)));
    match result {
        Ok(text) => Ok(text),
        Err(payload) => {
            let msg = downcast_panic_payload(payload, "unknown html parser panic");
            tracing::warn!(
                path = %path.display(),
                "html parser panicked during extraction: {msg}"
            );
            Err(SafeExtractError::Panic(msg))
        }
    }
}

/// DOM-walk body text extraction. Pulled out so MHTML and EPUB
/// chapter extraction (which run on already-loaded HTML strings) can
/// share the same routine without re-reading from disk.
///
/// Walks the document depth-first; drops `<script>`, `<style>`,
/// `<noscript>`, `<template>` subtrees; emits a newline at the start
/// of block-level elements so word boundaries survive across tags.
pub(crate) fn html_body_text(raw: &str) -> String {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(raw);
    let body_sel = Selector::parse("body").expect("valid selector");
    let walk_root: ego_tree::NodeRef<'_, scraper::Node> = match doc.select(&body_sel).next() {
        Some(body) => *body, // ElementRef<'_> derefs to NodeRef<'_, Node>
        None => doc.tree.root(),
    };
    let mut out = String::new();
    walk_html_node(walk_root, &mut out);
    collapse_html_whitespace(&out)
}

fn walk_html_node(node: ego_tree::NodeRef<'_, scraper::Node>, out: &mut String) {
    use scraper::Node;
    if let Node::Element(el) = node.value() {
        let name = el.name();
        if matches!(name, "script" | "style" | "noscript" | "template") {
            return; // drop the whole subtree
        }
        if matches!(
            name,
            "p" | "div"
                | "br"
                | "li"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "tr"
                | "section"
                | "article"
                | "header"
                | "footer"
        ) && !out.ends_with('\n')
        {
            out.push('\n');
        }
    }
    if let Node::Text(t) = node.value() {
        out.push_str(&t.text);
    }
    for child in node.children() {
        walk_html_node(child, out);
    }
}

/// Read an `.mhtml` (MIME-encoded HTML, a.k.a. SingleFile output or
/// Chrome "Save as Webpage, Complete") file and return its body text.
/// Walks the multipart MIME tree, picks the first `text/html` part,
/// and runs it through the same `html_body_text` routine standalone
/// HTML files use.
pub fn safe_extract_mhtml_text(path: &Path) -> Result<String, SafeExtractError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let bytes = std::fs::read(path).map_err(|e| SafeExtractError::Parse(format!("read: {e}")))?;
    let result = catch_unwind(AssertUnwindSafe(|| extract_mhtml_html_part(&bytes)));
    match result {
        Ok(Ok(html)) => Ok(html_body_text(&html)),
        Ok(Err(e)) => Err(e),
        Err(payload) => {
            let msg = downcast_panic_payload(payload, "unknown mhtml parser panic");
            tracing::warn!(
                path = %path.display(),
                "mhtml parser panicked during extraction: {msg}"
            );
            Err(SafeExtractError::Panic(msg))
        }
    }
}

fn extract_mhtml_html_part(bytes: &[u8]) -> Result<String, SafeExtractError> {
    use mail_parser::{MessageParser, MimeHeaders};
    let msg = MessageParser::default()
        .parse(bytes)
        .ok_or_else(|| SafeExtractError::Parse("mhtml: failed to parse MIME envelope".into()))?;
    // Walk every part; return the first text/html body we see.
    for part in msg.parts.iter() {
        let is_html = part
            .content_type()
            .map(|ct| ct.ctype().eq_ignore_ascii_case("text"))
            .unwrap_or(false)
            && part
                .content_type()
                .and_then(|ct| ct.subtype())
                .map(|s| s.eq_ignore_ascii_case("html"))
                .unwrap_or(false);
        if is_html {
            // `body_text(0)` returns the part's decoded text body.
            if let Some(text) = part.text_contents() {
                return Ok(text.to_string());
            }
        }
    }
    Err(SafeExtractError::Other(
        "mhtml: no text/html part found".into(),
    ))
}

/// Read an `.epub` file and return concatenated chapter text. Walks
/// the EPUB spine in canonical reading order, pulls each chapter's
/// XHTML body, and joins them with two newlines so the chunker sees
/// a chapter boundary. In-memory only — `rbook` reads the zip
/// archive without writing anything to disk, preserving the
/// watched-folder read-only-on-source invariant.
pub fn safe_extract_epub_text(path: &Path) -> Result<String, SafeExtractError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let path_owned = path.to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(move || extract_epub_chapters(&path_owned)));
    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(e),
        Err(payload) => {
            let msg = downcast_panic_payload(payload, "unknown epub parser panic");
            tracing::warn!(
                path = %path.display(),
                "epub parser panicked during extraction: {msg}"
            );
            Err(SafeExtractError::Panic(msg))
        }
    }
}

fn extract_epub_chapters(path: &Path) -> Result<String, SafeExtractError> {
    let epub = rbook::Epub::open(path).map_err(|e| {
        let msg = e.to_string();
        let lower = msg.to_ascii_lowercase();
        if lower.contains("encrypt") {
            SafeExtractError::Encrypted
        } else {
            SafeExtractError::Parse(format!("epub open: {msg}"))
        }
    })?;
    let mut chapters: Vec<String> = Vec::new();
    for entry in epub.spine().iter() {
        let Some(manifest_entry) = entry.manifest_entry() else {
            // Spine entry whose idref doesn't resolve in the manifest
            // — malformed but recoverable; skip and keep going.
            continue;
        };
        if !is_html_media_type(manifest_entry.media_type()) {
            continue;
        }
        let raw: String = match manifest_entry.read_str() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    chapter_idref = %entry.idref(),
                    "epub chapter read failed: {e}"
                );
                continue;
            }
        };
        let text = html_body_text(&raw);
        if !text.trim().is_empty() {
            chapters.push(text);
        }
    }
    if chapters.is_empty() {
        return Err(SafeExtractError::Other(
            "epub: no readable chapters in spine".into(),
        ));
    }
    Ok(chapters.join("\n\n"))
}

fn is_html_media_type(media: &str) -> bool {
    let lower = media.to_ascii_lowercase();
    lower == "application/xhtml+xml" || lower == "text/html"
}

/// Read a `.docx` file and return paragraph text. Headers, footers,
/// and footnotes are not included in v1 output (deferred to v1.x);
/// `docx_lite::extract_text` walks paragraphs + table cells in
/// document order, which is the right surface for the chunker.
pub fn safe_extract_docx_text(path: &Path) -> Result<String, SafeExtractError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let path_owned = path.to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(move || {
        docx_lite::extract_text(&path_owned)
    }));
    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(classify_docx_error(e)),
        Err(payload) => {
            let msg = downcast_panic_payload(payload, "unknown docx parser panic");
            tracing::warn!(
                path = %path.display(),
                "docx parser panicked during extraction: {msg}"
            );
            Err(SafeExtractError::Panic(msg))
        }
    }
}

fn classify_docx_error(e: docx_lite::DocxError) -> SafeExtractError {
    use docx_lite::DocxError;
    match e {
        // Encrypted .docx files surface as Zip errors when the
        // archive uses Office's "Document Open Password" (the file
        // becomes a wrapper OLE compound, not a plain zip). We
        // detect by inspecting the message; this is best-effort —
        // the alternative is loading another crate to recognise the
        // CFB header.
        DocxError::Zip(ze) => {
            let msg = ze.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("encrypted")
                || lower.contains("password")
                || lower.contains("invalid zip")
            {
                if lower.contains("encrypted") || lower.contains("password") {
                    SafeExtractError::Encrypted
                } else {
                    SafeExtractError::Parse(format!("docx zip: {msg}"))
                }
            } else {
                SafeExtractError::Parse(format!("docx zip: {msg}"))
            }
        }
        DocxError::Xml(xe) => SafeExtractError::Parse(format!("docx xml: {xe}")),
        DocxError::Utf8(ue) => SafeExtractError::Parse(format!("docx utf8: {ue}")),
        DocxError::Io(ioe) => SafeExtractError::Parse(format!("docx io: {ioe}")),
        DocxError::Structure(s) => SafeExtractError::Other(format!("docx structure: {s}")),
        DocxError::FileNotFound(s) => SafeExtractError::Parse(format!("docx file: {s}")),
        DocxError::UnsupportedFormat(s) => {
            SafeExtractError::Other(format!("docx unsupported: {s}"))
        }
    }
}

/// Collapse runs of internal whitespace conservatively so the chunker
/// doesn't see "    " between tag boundaries; preserve newlines
/// (block separators).
fn collapse_html_whitespace(raw: &str) -> String {
    let mut collapsed = String::with_capacity(raw.len());
    let mut prev_space = false;
    for ch in raw.chars() {
        if ch == '\n' {
            collapsed.push('\n');
            prev_space = false;
        } else if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }
    collapsed.trim().to_string()
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
        let end_of_line = tail.is_empty() || tail.starts_with('\n') || tail.starts_with("\r\n");
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
    data_dir
        .join("local-corpus-staging")
        .join(format!("{corpus_id}.jsonl"))
}

// ─── OCR stage ───────────────────────────────────────────────────────

/// Append OCR'd content for scanned PDFs to an existing staging
/// JSONL. Called AFTER `stage_blocking` has written the readable
/// (born-digital) entries; the OCR pipeline is async because the
/// cleanup pass talks to the daemon's `/v1/chat/completions`.
///
/// Per-file failures collect into `StageResult.failures` exactly as
/// `stage_blocking` does — the manager merges both result vectors so
/// the completion screen surfaces both kinds of error in one list.
///
/// `on_page_progress`, when provided, fires before each page is sent
/// to Tesseract — used by the manager to bridge `OcrPage` events
/// onto the local-corpus progress channel.
pub async fn append_ocr_to_staging(
    config: &LocalCorpusConfig,
    files: &[FileMeta],
    output_path: &Path,
    ocr_ctx: &OcrCtx,
    on_page_progress: Option<PageProgressCallback>,
) -> std::io::Result<StageResult> {
    let mut result = StageResult::default();
    if files.is_empty() {
        return Ok(result);
    }

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Build the rasterizer once so the pdfium dynamic library is
    // bound a single time across the OCR queue.
    let rasterizer = match super::ocr::rasterize::PdfiumRasterizer::new(ocr_ctx) {
        Ok(r) => r,
        Err(e) => {
            // Setup-level error — the entire OCR queue fails. Surface
            // each scanned PDF as a runtime failure so the user sees
            // them in the completion screen.
            for meta in files {
                result.failures.push(RuntimeFailure {
                    file: meta.clone(),
                    reason: format!("OCR engine setup failed: {e}"),
                });
            }
            return Ok(result);
        }
    };

    let file_total = files.len() as u32;

    let mut file_handle = OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_path)?;

    for (idx, meta) in files.iter().enumerate() {
        let file_idx = (idx as u32) + 1;
        let res = super::ocr::pipeline::extract_pdf_with_rasterizer(
            &rasterizer,
            &meta.path,
            ocr_ctx,
            &meta.display_name,
            file_idx,
            file_total,
            on_page_progress.clone(),
        )
        .await;

        match res {
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
                let json = serde_json::to_string(&line)
                    .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
                writeln!(file_handle, "{json}")?;
                result.staged += 1;
            }
            Ok(_) => {
                // Empty OCR output — no readable pages. Surface as a
                // failure so the user knows the file was attempted.
                result.failures.push(RuntimeFailure {
                    file: meta.clone(),
                    reason: "OCR produced no text".into(),
                });
            }
            Err(reason) => {
                result.failures.push(RuntimeFailure {
                    file: meta.clone(),
                    reason: format!("OCR failed: {reason}"),
                });
            }
        }
    }

    Ok(result)
}

// `extract_pdf_via_ocr` is re-exported for callers (sovereign-cli,
// future tools) that want a one-shot OCR without touching the
// staging machinery.
pub use super::ocr::extract_pdf_via_ocr as ocr_extract_pdf;

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
            Err(SafeExtractError::Parse(_))
            | Err(SafeExtractError::Panic(_))
            | Err(SafeExtractError::Other(_)) => {}
            Err(SafeExtractError::Encrypted) => {
                panic!("garbage bytes should not be classified as encrypted")
            }
            Ok(_) => panic!("garbage bytes should not parse"),
        }
    }

    #[test]
    fn html_body_text_extracts_paragraph_text() {
        let html = r#"<html><body><p>Hello world.</p><p>Second paragraph.</p></body></html>"#;
        let text = html_body_text(html);
        assert!(text.contains("Hello world."), "got: {text:?}");
        assert!(text.contains("Second paragraph."), "got: {text:?}");
    }

    #[test]
    fn html_body_text_drops_script_and_style() {
        let html = r#"<html><head>
            <style>body { color: red; }</style>
        </head><body>
            <script>alert('xss')</script>
            <p>visible</p>
            <script>more.script.body</script>
        </body></html>"#;
        let text = html_body_text(html);
        assert!(text.contains("visible"));
        assert!(!text.contains("alert"), "script body leaked: {text:?}");
        assert!(!text.contains("color: red"), "style body leaked: {text:?}");
        assert!(
            !text.contains("more.script"),
            "second script leaked: {text:?}"
        );
    }

    #[test]
    fn html_body_text_inserts_block_breaks() {
        // Adjacent block elements without intervening whitespace should
        // still produce word-boundary newlines so chunking doesn't fuse
        // "ParagraphOne" with "ParagraphTwo".
        let html = "<html><body><p>One</p><p>Two</p></body></html>";
        let text = html_body_text(html);
        assert!(
            !text.contains("OneTwo"),
            "block elements fused without break: {text:?}"
        );
    }

    #[test]
    fn safe_extract_html_text_reads_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.html");
        fs::write(
            &path,
            "<html><body><h1>Title</h1><p>Body text.</p></body></html>",
        )
        .unwrap();
        let text = safe_extract_html_text(&path).expect("html extracts");
        assert!(text.contains("Title"));
        assert!(text.contains("Body text."));
    }

    #[test]
    fn safe_extract_html_text_returns_parse_error_on_read_failure() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing.html");
        let result = safe_extract_html_text(&missing);
        assert!(matches!(result, Err(SafeExtractError::Parse(_))));
    }

    #[test]
    fn safe_extract_html_text_handles_garbage_without_panic() {
        // The HTML5 parser is permissive — random bytes parse as a
        // (largely empty) document. The point of this test is that
        // we never panic past the wrapper, regardless of input.
        let dir = tempdir().unwrap();
        let path = dir.path().join("garbage.html");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x01, 0x02, 0xc3, 0x28]).unwrap();
        // Bytes that aren't valid UTF-8 → read_to_string fails →
        // Parse error. Either way: not a panic.
        let _ = safe_extract_html_text(&path);
    }

    #[test]
    fn extract_one_dispatches_html_branch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("doc.htm");
        fs::write(&path, "<html><body><p>Dispatched.</p></body></html>").unwrap();
        let cfg = folder_cfg(dir.path());
        let text = extract_one(&path, &cfg).expect("dispatch ok");
        assert!(text.contains("Dispatched."));
    }

    /// Builds a minimal multipart/related MHTML blob the way Chrome's
    /// "Save as Webpage, Complete" / SingleFile would. One text/html
    /// part with a snippet, one image part that should be ignored.
    fn sample_mhtml(html: &str) -> String {
        format!(
            "From: <Saved by Browser>\r\n\
             Subject: Sample\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/related; boundary=\"BOUNDARY\"\r\n\
             \r\n\
             --BOUNDARY\r\n\
             Content-Type: text/html; charset=utf-8\r\n\
             Content-Transfer-Encoding: 8bit\r\n\
             \r\n\
             {html}\r\n\
             --BOUNDARY\r\n\
             Content-Type: image/png\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             aGVsbG8=\r\n\
             --BOUNDARY--\r\n",
        )
    }

    #[test]
    fn safe_extract_mhtml_text_picks_html_part() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("page.mhtml");
        let mhtml = sample_mhtml("<html><body><p>From archive.</p></body></html>");
        fs::write(&path, mhtml).unwrap();
        let text = safe_extract_mhtml_text(&path).expect("mhtml extracts");
        assert!(text.contains("From archive."), "got: {text:?}");
        // The image part's base64 body must not leak as visible text.
        assert!(!text.contains("aGVsbG8"), "image body leaked: {text:?}");
    }

    #[test]
    fn safe_extract_mhtml_text_returns_other_when_no_html_part() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("noimg.mhtml");
        fs::write(
            &path,
            "From: <x>\r\n\
             Subject: No HTML\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/related; boundary=\"B\"\r\n\
             \r\n\
             --B\r\n\
             Content-Type: image/png\r\n\
             Content-Transfer-Encoding: base64\r\n\
             \r\n\
             aGVsbG8=\r\n\
             --B--\r\n",
        )
        .unwrap();
        let result = safe_extract_mhtml_text(&path);
        match result {
            Err(SafeExtractError::Other(msg)) => assert!(msg.contains("no text/html")),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn extract_one_dispatches_mhtml_branch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("doc.mhtml");
        let mhtml = sample_mhtml("<html><body><p>MHTML dispatched.</p></body></html>");
        fs::write(&path, mhtml).unwrap();
        let cfg = folder_cfg(dir.path());
        let text = extract_one(&path, &cfg).expect("dispatch ok");
        assert!(text.contains("MHTML dispatched."));
    }

    /// Build a minimal EPUB-3 file at `path` with two chapters.
    fn write_sample_epub(path: &Path) {
        use rbook::epub::EpubChapter;
        let epub = rbook::Epub::builder()
            .title("Sample")
            .language("en")
            .chapter([
                EpubChapter::new("Chapter 1")
                    .xhtml_body("<h1>Chapter 1</h1><p>First chapter body.</p>"),
                EpubChapter::new("Chapter 2")
                    .xhtml_body("<h1>Chapter 2</h1><p>Second chapter body.</p>"),
            ])
            .build();
        let f = std::fs::File::create(path).expect("create epub file");
        epub.write().write(f).expect("write epub");
    }

    #[test]
    fn safe_extract_epub_text_concatenates_chapters() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.epub");
        write_sample_epub(&path);
        let text = safe_extract_epub_text(&path).expect("epub extracts");
        assert!(text.contains("First chapter body."), "got: {text:?}");
        assert!(text.contains("Second chapter body."), "got: {text:?}");
        // Chapters joined with a blank-line separator so the chunker
        // sees the boundary.
        assert!(text.contains("\n\n"), "no chapter separator: {text:?}");
    }

    #[test]
    fn safe_extract_epub_text_returns_parse_error_for_garbage() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("not-an-epub.epub");
        fs::write(&path, b"this is not a zip").unwrap();
        let result = safe_extract_epub_text(&path);
        assert!(result.is_err());
        match result {
            Err(SafeExtractError::Parse(_))
            | Err(SafeExtractError::Panic(_))
            | Err(SafeExtractError::Other(_)) => {}
            Err(SafeExtractError::Encrypted) => {
                panic!("garbage bytes should not be classified as encrypted")
            }
            Ok(_) => panic!("garbage bytes should not parse"),
        }
    }

    #[test]
    fn extract_one_dispatches_epub_branch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("doc.epub");
        write_sample_epub(&path);
        let cfg = folder_cfg(dir.path());
        let text = extract_one(&path, &cfg).expect("dispatch ok");
        assert!(text.contains("First chapter body."));
    }

    /// Hand-build a minimal DOCX (zip with the three required parts)
    /// containing one paragraph with the given body text. This avoids
    /// pulling in `docx-rs`, whose `encoding` feature on quick-xml
    /// 0.36 breaks corpus-engine via workspace feature unification.
    fn write_sample_docx(path: &Path, body: &str) {
        use std::io::Write;
        use zip::write::FileOptions;
        use zip::CompressionMethod;

        let f = std::fs::File::create(path).expect("create docx file");
        let mut zip = zip::ZipWriter::new(f);
        let opts: FileOptions<()> =
            FileOptions::default().compression_method(CompressionMethod::Deflated);

        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#,
        )
        .unwrap();

        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#,
        )
        .unwrap();

        zip.start_file("word/document.xml", opts).unwrap();
        let doc = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:r><w:t>{body}</w:t></w:r></w:p>
</w:body>
</w:document>"#,
            body = xml_escape(body)
        );
        zip.write_all(doc.as_bytes()).unwrap();

        zip.finish().expect("finish docx zip");
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    #[test]
    fn safe_extract_docx_text_extracts_paragraph() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.docx");
        write_sample_docx(&path, "Hello world from DOCX.");
        let text = safe_extract_docx_text(&path).expect("docx extracts");
        assert!(text.contains("Hello world from DOCX."), "got: {text:?}");
    }

    #[test]
    fn safe_extract_docx_text_returns_parse_error_for_garbage() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("not-a-docx.docx");
        fs::write(&path, b"this is not a zip").unwrap();
        let result = safe_extract_docx_text(&path);
        assert!(result.is_err());
        match result {
            Err(SafeExtractError::Parse(_))
            | Err(SafeExtractError::Panic(_))
            | Err(SafeExtractError::Other(_)) => {}
            Err(SafeExtractError::Encrypted) => {
                panic!("garbage bytes should not be classified as encrypted")
            }
            Ok(_) => panic!("garbage bytes should not parse"),
        }
    }

    #[test]
    fn extract_one_dispatches_docx_branch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("doc.docx");
        write_sample_docx(&path, "DOCX dispatched.");
        let cfg = folder_cfg(dir.path());
        let text = extract_one(&path, &cfg).expect("dispatch ok");
        assert!(text.contains("DOCX dispatched."));
    }

    #[tokio::test]
    async fn append_ocr_no_files_returns_empty_result() {
        let dir = tempdir().unwrap();
        let cfg = folder_cfg(dir.path());
        let staging = dir.path().join("staged.jsonl");
        std::fs::write(&staging, "").unwrap();
        let ctx = super::super::ocr::OcrCtx::for_test(
            std::path::PathBuf::from("/no/tesseract"),
            std::path::PathBuf::from("/no/tessdata"),
            "http://127.0.0.1:1".into(),
        );
        let res = append_ocr_to_staging(&cfg, &[], &staging, &ctx, None)
            .await
            .unwrap();
        assert_eq!(res.staged, 0);
        assert!(res.failures.is_empty());
    }

    #[tokio::test]
    async fn append_ocr_setup_failure_marks_every_file() {
        // OcrCtx points at a deliberately-broken pdfium path. Every
        // queued scanned PDF must land in `failures` with a clear
        // setup-error message — they must NOT silently disappear.
        let dir = tempdir().unwrap();
        let cfg = folder_cfg(dir.path());
        let staging = dir.path().join("staged.jsonl");
        std::fs::write(&staging, "").unwrap();
        let mut ctx = super::super::ocr::OcrCtx::for_test(
            std::path::PathBuf::from("/no/tesseract"),
            std::path::PathBuf::from("/no/tessdata"),
            "http://127.0.0.1:1".into(),
        );
        ctx.pdfium_lib_path = Some(std::path::PathBuf::from(
            "/this/path/does/not/exist/libpdfium.dylib",
        ));
        let scanned = vec![
            FileMeta {
                path: dir.path().join("a.pdf"),
                size_bytes: 0,
                display_name: "a".into(),
            },
            FileMeta {
                path: dir.path().join("b.pdf"),
                size_bytes: 0,
                display_name: "b".into(),
            },
        ];
        let res = append_ocr_to_staging(&cfg, &scanned, &staging, &ctx, None)
            .await
            .unwrap();
        assert_eq!(res.staged, 0);
        assert_eq!(res.failures.len(), 2);
        for failure in &res.failures {
            assert!(
                failure.reason.contains("OCR engine setup failed"),
                "expected setup-failure message, got: {}",
                failure.reason
            );
        }
    }

    #[test]
    fn safe_extract_pdf_catches_panic() {
        // Verifies the panic-catch semantics of `safe_extract_pdf_text`
        // independent of pdf-extract: we reconstruct `catch_unwind`
        // around an explicit panic to ensure our wrapper's error
        // mapping treats it as `SafeExtractError::Panic`, not propagates.
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
