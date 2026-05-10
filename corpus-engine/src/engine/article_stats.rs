//! Sample-based statistics for a corpus's extracted JSONL file.
//!
//! The Wikipedia resume-path UX needs a completion percent that's
//! monotonic and approximately right. A full 74 GB scan to count
//! sections costs 30–120 s; sampling the first ~100 MB gets us the
//! same information to ±20 % in 1–2 s, which is imperceptible on a
//! progress bar updating once per second.
//!
//! The estimate has two outputs:
//!   - `total_articles`: number of JSONL lines in the file. Derived
//!     by extrapolating sample-line-count by (file_size / sample_size).
//!   - `mean_sections_per_article`: average number of sections
//!     emitted by the Wikipedia extractor per article. Computed by
//!     parsing each sampled line and counting entries in the
//!     `sections` array.
//!
//! Product of the two is the total-sections estimate, which is the
//! right denominator for `committed_iter_pos` in the Wikipedia-style
//! pipeline (each iter step emits one section or lead chunk).
//!
//! Results are cached in two places:
//!   1. In-memory on the engine so repeated calls within a session
//!      return instantly.
//!   2. On-disk sidecar `<corpus>.extracted.jsonl.count` so daemon
//!      restarts are free too. Invalidated by comparing the source
//!      file's (mtime, size) against the sidecar's recorded values.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Approximate size of the byte prefix we read + parse to estimate
/// the full file's article count and mean sections-per-article.
/// 100 MB is large enough to smooth out per-article size variance
/// (Wikipedia has everything from 500-byte stubs to 200 KB megas)
/// and small enough to fit easily in the OS page cache for subsequent
/// passes by the real extractor.
const SAMPLE_BYTES: u64 = 100 * 1024 * 1024;

/// Minimum number of articles in the sample before we trust the
/// extrapolation. On a sufficiently tiny fixture (smaller than
/// `SAMPLE_BYTES`) the whole file is scanned and this is irrelevant,
/// so the check only matters when the first 100 MB contained almost
/// no articles — highly unlikely for real corpora.
const MIN_SAMPLE_ARTICLES: u64 = 10;

/// Persistent snapshot of the sampler's output. Matches the on-disk
/// sidecar JSON shape exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleStats {
    /// Estimated total number of JSONL lines (articles) in the
    /// extracted file. For tiny files where the whole thing was
    /// scanned, this is exact.
    pub total_articles: u64,
    /// Mean sections per article in the sample. `1.0` means each
    /// article contributes one extracted doc (e.g. lead-only); `2.5`
    /// means 2.5 sections on average, typical of Wikipedia.
    pub mean_sections_per_article: f64,
    /// Product of the two above — the best denominator for
    /// `committed_iter_pos / total_sections_estimate` percents.
    pub total_sections_estimate: u64,
    /// Source-file mtime (unix seconds) captured at sample time.
    /// Used for cache invalidation.
    pub source_mtime_secs: u64,
    /// Source-file size in bytes captured at sample time.
    pub source_size_bytes: u64,
    /// When the sample ran, unix seconds. Purely diagnostic.
    pub sampled_at_secs: u64,
}

impl ArticleStats {
    /// Returns `true` when this cached snapshot was generated from a
    /// source file whose `(mtime, size)` still matches. Any drift
    /// invalidates the estimate — the file has been re-extracted or
    /// appended to since we sampled.
    pub fn matches_source(&self, path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        size == self.source_size_bytes && mtime == self.source_mtime_secs
    }
}

/// Compute (or recompute) `ArticleStats` for the extracted JSONL at
/// `source_path`. Writes a sidecar next to the source so repeated
/// callers can skip the sampling pass entirely.
///
/// **Blocking**: does synchronous I/O. Callers on a Tokio runtime
/// should wrap this in `tokio::task::spawn_blocking`.
pub fn sample_article_stats(source_path: &Path) -> Result<ArticleStats> {
    let meta = std::fs::metadata(source_path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("sample_article_stats: stat {}: {e}", source_path.display()),
        ))
    })?;
    let source_size_bytes = meta.len();
    let source_mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let sampled_at_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if source_size_bytes == 0 {
        // Empty file — nothing to extrapolate from.
        let stats = ArticleStats {
            total_articles: 0,
            mean_sections_per_article: 0.0,
            total_sections_estimate: 0,
            source_mtime_secs,
            source_size_bytes,
            sampled_at_secs,
        };
        write_sidecar(source_path, &stats)?;
        return Ok(stats);
    }

    let file = std::fs::File::open(source_path).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("sample_article_stats: open {}: {e}", source_path.display()),
        ))
    })?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    let budget = SAMPLE_BYTES.min(source_size_bytes);
    let mut bytes_read: u64 = 0;
    let mut line_buf = Vec::with_capacity(256 * 1024);
    let mut articles_in_sample: u64 = 0;
    let mut section_count_total: u64 = 0;

    while bytes_read < budget {
        line_buf.clear();
        let n = reader.read_until(b'\n', &mut line_buf).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("sample_article_stats: read {}: {e}", source_path.display()),
            ))
        })?;
        if n == 0 {
            // Hit EOF before the budget — tiny fixture case. We'll
            // treat the sample as the entire file, so the extrapolation
            // becomes an exact count.
            break;
        }
        bytes_read += n as u64;

        let trimmed = line_buf.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        articles_in_sample += 1;
        // Best-effort section count. A parse failure (non-JSON line,
        // corruption, partial write) falls back to 1 so the sampler
        // degrades gracefully rather than aborting with an error that
        // would leave the UI stuck at "unknown".
        match serde_json::from_slice::<serde_json::Value>(trimmed) {
            Ok(v) => {
                let sections = v
                    .get("sections")
                    .and_then(|s| s.as_array())
                    .map(|a| a.len() as u64)
                    // Lead counts as one implicit section when present.
                    .unwrap_or(0);
                // `+ 1` models the always-present lead/abstract chunk
                // the Wikipedia extractor emits before iterating
                // sections. For non-Wikipedia JSONLs (title/text
                // without structure) the pipeline emits ~1 doc per
                // line anyway so this is a reasonable default.
                section_count_total += sections + 1;
            }
            Err(_) => section_count_total += 1,
        }
    }

    // Guard against degenerate samples (e.g. an empty or heavily
    // whitespace-padded file). Falling back to `total_articles = lines
    // read` keeps the number non-zero but prevents a wild extrapolation.
    if articles_in_sample < MIN_SAMPLE_ARTICLES {
        let stats = ArticleStats {
            total_articles: articles_in_sample,
            mean_sections_per_article: if articles_in_sample == 0 {
                0.0
            } else {
                section_count_total as f64 / articles_in_sample as f64
            },
            total_sections_estimate: section_count_total,
            source_mtime_secs,
            source_size_bytes,
            sampled_at_secs,
        };
        write_sidecar(source_path, &stats)?;
        return Ok(stats);
    }

    let mean_sections_per_article =
        section_count_total as f64 / articles_in_sample as f64;

    let total_articles = if bytes_read >= source_size_bytes {
        // Whole file fit inside the sample budget — exact count.
        articles_in_sample
    } else {
        // Extrapolate. `bytes_read` may be slightly under the budget
        // because we always stop after a complete line; dividing by
        // `bytes_read` (actual) instead of the nominal budget keeps
        // the estimate unbiased.
        let ratio = source_size_bytes as f64 / bytes_read as f64;
        (articles_in_sample as f64 * ratio).round() as u64
    };

    let total_sections_estimate =
        (total_articles as f64 * mean_sections_per_article).round() as u64;

    let stats = ArticleStats {
        total_articles,
        mean_sections_per_article,
        total_sections_estimate,
        source_mtime_secs,
        source_size_bytes,
        sampled_at_secs,
    };

    write_sidecar(source_path, &stats)?;
    Ok(stats)
}

/// Deterministic path for the sidecar file: same directory as the
/// source, `.count` extension appended. Living beside the source
/// means an admin rm-ing the extracted JSONL also clears the
/// sidecar, which is the correct behaviour.
pub fn sidecar_path(source_path: &Path) -> PathBuf {
    let mut p = source_path.to_path_buf();
    if let Some(ext) = p.extension().and_then(|e| e.to_str()).map(String::from) {
        p.set_extension(format!("{ext}.count"));
    } else {
        p.set_extension("count");
    }
    p
}

/// Read a sidecar, if it exists AND matches the current source file.
pub fn read_sidecar(source_path: &Path) -> Option<ArticleStats> {
    let sidecar = sidecar_path(source_path);
    let raw = std::fs::read_to_string(&sidecar).ok()?;
    let stats: ArticleStats = serde_json::from_str(&raw).ok()?;
    if stats.matches_source(source_path) {
        Some(stats)
    } else {
        None
    }
}

fn write_sidecar(source_path: &Path, stats: &ArticleStats) -> Result<()> {
    let sidecar = sidecar_path(source_path);
    let json = serde_json::to_string_pretty(stats)
        .map_err(|e| Error::Serialization(format!("write sidecar: {e}")))?;
    // Write atomically via temp + rename so a crash mid-write leaves
    // either the old sidecar or nothing — never a half-parsed one.
    let tmp = sidecar.with_extension("count.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &sidecar)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_sample_jsonl(path: &Path, article_count: usize, sections_each: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        for i in 0..article_count {
            let sections: Vec<serde_json::Value> = (0..sections_each)
                .map(|k| {
                    serde_json::json!({
                        "name": format!("Section {k}"),
                        "type": "section",
                        "has_parts": [{
                            "type": "paragraph",
                            "value": "Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                                      Padding padding padding padding padding padding padding.",
                            "links": []
                        }]
                    })
                })
                .collect();
            let line = serde_json::json!({
                "name": format!("Article {i}"),
                "identifier": i,
                "abstract": format!("Abstract for article {i} with enough content to fill a chunk."),
                "url": format!("https://en.wikipedia.org/wiki/Article_{i}"),
                "sections": sections
            });
            writeln!(f, "{}", line).unwrap();
        }
    }

    #[test]
    fn tiny_file_is_scanned_exhaustively() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("tiny.jsonl");
        write_sample_jsonl(&src, 25, 4);

        let stats = sample_article_stats(&src).unwrap();
        assert_eq!(stats.total_articles, 25, "small-file count must be exact");
        // 4 sections + 1 implicit lead = 5 docs per article.
        assert!(
            (stats.mean_sections_per_article - 5.0).abs() < 1e-6,
            "mean sections off: {:?}",
            stats
        );
        assert_eq!(stats.total_sections_estimate, 125);
    }

    #[test]
    fn sidecar_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("round.jsonl");
        write_sample_jsonl(&src, 12, 2);

        let first = sample_article_stats(&src).unwrap();
        // Sidecar written next to source.
        let sidecar = sidecar_path(&src);
        assert!(sidecar.exists());

        let loaded = read_sidecar(&src).expect("sidecar should load");
        assert_eq!(loaded.total_articles, first.total_articles);
        assert_eq!(
            loaded.mean_sections_per_article,
            first.mean_sections_per_article
        );
    }

    #[test]
    fn sidecar_invalidates_on_size_change() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("changes.jsonl");
        write_sample_jsonl(&src, 5, 1);

        let _ = sample_article_stats(&src).unwrap();
        assert!(read_sidecar(&src).is_some());

        // Append data → size changes → sidecar stale.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&src)
            .unwrap();
        writeln!(f, "{{\"name\":\"added\",\"sections\":[]}}").unwrap();
        drop(f);

        assert!(
            read_sidecar(&src).is_none(),
            "sidecar should invalidate when source grows"
        );
    }

    #[test]
    fn empty_file_yields_zero_stats() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("empty.jsonl");
        std::fs::File::create(&src).unwrap();

        let stats = sample_article_stats(&src).unwrap();
        assert_eq!(stats.total_articles, 0);
        assert_eq!(stats.total_sections_estimate, 0);
    }

    #[test]
    fn sidecar_path_composes_correctly() {
        assert_eq!(
            sidecar_path(Path::new("/tmp/wikipedia.extracted.jsonl")),
            PathBuf::from("/tmp/wikipedia.extracted.jsonl.count"),
        );
        assert_eq!(
            sidecar_path(Path::new("/tmp/noextension")),
            PathBuf::from("/tmp/noextension.count"),
        );
    }
}
