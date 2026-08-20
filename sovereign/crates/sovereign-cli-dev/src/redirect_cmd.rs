// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code redirect` — rewrite every reference to a symbol, from the graph.
//!
//! The missing primitive in the convergence chain. `converge` finds duplicated
//! IDENTITY and `dry-report` finds duplicated BEHAVIOUR, but both stop at a
//! worklist: they name the twins and leave the migration to a human editing
//! call sites by hand. That gap is why redundant-but-live code survives — the
//! losers of a convergence cannot be deleted until every caller points at the
//! winner, and "edit 400 call sites correctly" is the step that never gets
//! attempted.
//!
//! # Why this can be exact
//!
//! rust-analyzer emits a character range for every occurrence. Until
//! 2026-08-20 the SCIP ingest decoded `occ.range`, kept the line, and threw the
//! columns away — every reference in the graph was a line pointer, which is
//! enough to COUNT callers and not enough to REWRITE them. The refs table now
//! carries `start_col`/`end_line`/`end_col`, so a reference is an exact span
//! and the rewrite is a slice replacement rather than a regex guess. That also
//! means this sees what grep cannot: trait dispatch, aliased imports, and
//! macro-expanded call sites are all compiler-resolved occurrences.
//!
//! # Three refusals, on purpose
//!
//! This edits source files, so it is built to decline rather than to try:
//!
//! 1. **A stale graph is not rewritable.** Spans describe the commit that was
//!    indexed. If indexed-source files changed since, a span may now point at
//!    different characters — rewriting from it corrupts the file. Reuses
//!    `converge_cmd`'s freshness assessment (one decider, §10.6) and exits 3
//!    rather than guessing.
//! 2. **A span must still say what the graph claims.** Before touching a site,
//!    the text under the span is compared to the symbol being redirected. A
//!    mismatch means the graph and the file disagree; that site is skipped and
//!    counted, never rewritten on faith. This is the instrument validating
//!    itself against the artifact (§18.4).
//! 3. **A row with no span is not a site.** Pre-migration rows read `-1`
//!    (`ScipRefRecord::has_span`). They are reported as unrewritable with the
//!    re-index command, never silently treated as column 0 — which would splice
//!    the replacement into the head of the line.
//!
//! Dry-run is the default. `--apply` is the only thing that writes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use corpus_engine_scip::converge::{type_defs, SourceScope};
use corpus_engine_scip::scip_graph::ScipRefRecord;
use corpus_engine_scip::ScipGraph;

use crate::converge_cmd::assess_lag;

const HELP: &str = "\
svrn code redirect <symbol> --to <name> [options]

Rewrite every reference to <symbol> so it names <name> instead, using the
character spans in the SCIP graph. Compiler-resolved, so it reaches trait
dispatch and aliased imports that grep cannot see.

  <symbol>              the identifier as it appears at call sites
  --to <name>           the replacement identifier (required)
  --corpus-id <id>      default: the sole indexed code corpus
  --scope <prefix>      only rewrite sites under this repo-relative prefix
  --apply               write the files (default: dry run, prints the sites)
  --json                machine-readable summary

This does NOT update the definition, the imports, or the callee's own module —
it redirects CALL SITES. Run it, then let the compiler name what is left; that
residue is the real migration and it is much smaller than the call-site sweep.

exit 0 done (or dry run) · 1 error · 3 the graph cannot speak for this commit
(re-index first) · 4 nothing to rewrite
";

/// Exit codes, spelled once — same four-verdict contract as `converge status`
/// (§18.2). A run that rewrote nothing is NOT a pass.
const DONE: i32 = 0;
const ERROR: i32 = 1;
const CANNOT_JUDGE: i32 = 3;
const NOTHING: i32 = 4;

/// One rewritable occurrence, resolved to exact characters.
struct Site {
    file: String,
    /// 0-based, as SCIP records them.
    line: i32,
    start_col: i32,
    end_col: i32,
}

/// Why a reference was not rewritten. Counted and reported — never dropped.
#[derive(Default)]
struct Skipped {
    /// Row predates the span migration (`-1`). Fix is a re-index.
    no_span: usize,
    /// Span points outside the file — graph describes a different revision.
    out_of_range: Vec<String>,
    /// Text under the span is not the symbol. Graph and file disagree.
    mismatch: Vec<String>,
    /// Multi-line occurrence; this rewriter only handles single-line spans.
    multiline: usize,
}

pub async fn run(args: &[String]) -> i32 {
    let mut symbol: Option<String> = None;
    let mut to: Option<String> = None;
    let mut corpus_id: Option<String> = None;
    let mut scope_prefix: Option<String> = None;
    let mut apply = false;
    let mut json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--to" => {
                i += 1;
                to = args.get(i).cloned();
                if to.is_none() {
                    eprintln!("error: --to requires a value");
                    return ERROR;
                }
            }
            "--corpus-id" => {
                i += 1;
                corpus_id = args.get(i).cloned();
            }
            "--scope" => {
                i += 1;
                scope_prefix = args.get(i).cloned();
            }
            "--apply" => apply = true,
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return DONE;
            }
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                return ERROR;
            }
            positional => {
                if symbol.is_none() {
                    symbol = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    let (Some(symbol), Some(to)) = (symbol, to) else {
        println!("{HELP}");
        return ERROR;
    };
    if symbol == to {
        eprintln!("error: --to is the same identifier as <symbol>; nothing to do");
        return ERROR;
    }
    if !is_plain_ident(&to) {
        eprintln!("error: --to must be a plain identifier (got `{to}`)");
        return ERROR;
    }

    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    let corpus_id = match resolve_corpus(corpus_id, &indexes_dir) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let db_path = indexes_dir.join(&corpus_id).join("scip_graph.db");
    if !db_path.exists() {
        eprintln!(
            "error: no SCIP graph at {} — run `svrn project init` first",
            db_path.display()
        );
        return ERROR;
    }
    let graph = match ScipGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: cannot open SCIP graph: {e}");
            return ERROR;
        }
    };

    // ── Refusal 1: a stale graph is not rewritable ──────────────────────────
    // Same assessment `converge status` reports on, from the same graph handle,
    // so the freshness verdict and the spans can never be about different
    // commits. Counting from a stale graph is merely wrong; REWRITING from one
    // corrupts source, so this is a hard stop rather than a caveat line.
    let scope = SourceScope::default();
    let symbols = match graph.iter_all_symbols().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading symbols: {e}");
            return ERROR;
        }
    };
    let defs = type_defs(&symbols, &scope);
    let lag = assess_lag(graph.last_indexed_head().await, &defs, &scope);
    if !lag.can_judge() {
        eprint!("{}", lag.render(&corpus_id));
        eprintln!(
            "\nCANNOT-JUDGE — refusing to rewrite from a graph that does not describe this \
             commit. A span recorded against an older revision can point at different \
             characters now, and applying it would corrupt the file."
        );
        return CANNOT_JUDGE;
    }

    let refs = match graph.iter_all_refs().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: reading refs: {e}");
            return ERROR;
        }
    };

    let source_root = registered_root(&corpus_id)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let (sites, skipped, considered) =
        collect_sites(&refs, &symbol, scope_prefix.as_deref(), &source_root);

    if considered == 0 {
        eprintln!(
            "NOTHING — no reference to `{symbol}` in corpus `{corpus_id}`.\n\
             Check the name as it appears at CALL SITES (this matches the occurrence, \
             not the definition path)."
        );
        return NOTHING;
    }

    let by_file = group_by_file(sites);
    let mut rewritten = 0usize;
    let mut files_touched: Vec<String> = Vec::new();

    for (file, mut spans) in by_file {
        // Descending by (line, col): later edits first, so earlier spans keep
        // their offsets. Rewriting forward would shift every subsequent column
        // on the same line by the length delta.
        spans.sort_by(|a, b| b.line.cmp(&a.line).then(b.start_col.cmp(&a.start_col)));
        let abs = source_root.join(&file);
        let Ok(content) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
        let mut file_edits = 0usize;
        for s in &spans {
            let Some(line) = lines.get_mut(s.line as usize) else {
                continue;
            };
            let (Some(start), Some(end)) = (
                char_to_byte(line, s.start_col as usize),
                char_to_byte(line, s.end_col as usize),
            ) else {
                continue;
            };
            line.replace_range(start..end, &to);
            file_edits += 1;
        }
        if file_edits == 0 {
            continue;
        }
        rewritten += file_edits;
        files_touched.push(format!("{file} ({file_edits})"));
        if apply {
            // Preserve the file's trailing newline; `lines()` drops it.
            let mut out = lines.join("\n");
            if content.ends_with('\n') {
                out.push('\n');
            }
            if let Err(e) = std::fs::write(&abs, out) {
                eprintln!("error: writing {}: {e}", abs.display());
                return ERROR;
            }
        }
    }

    if json {
        let payload = serde_json::json!({
            "symbol": symbol,
            "to": to,
            "corpus_id": corpus_id,
            "applied": apply,
            "considered": considered,
            "rewritten": rewritten,
            "files": files_touched,
            "skipped_no_span": skipped.no_span,
            "skipped_mismatch": skipped.mismatch.len(),
            "skipped_out_of_range": skipped.out_of_range.len(),
            "skipped_multiline": skipped.multiline,
            "freshness": lag.verdict_word(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
    } else {
        println!(
            "{} `{symbol}` -> `{to}` in corpus `{corpus_id}`",
            if apply { "REWROTE" } else { "DRY RUN" }
        );
        println!("  {considered} reference(s) considered, {rewritten} rewritable");
        for f in &files_touched {
            println!("    {f}");
        }
        report_skips(&skipped, &corpus_id);
        if !apply && rewritten > 0 {
            println!("\n  nothing written. Re-run with --apply to write these {rewritten} edit(s).");
        }
    }

    if rewritten == 0 {
        return NOTHING;
    }
    DONE
}

/// Resolve every reference to `symbol` into an exact, verified span.
///
/// Returns `(sites, skipped, considered)` — `considered` counts every reference
/// that named the symbol, so the caller can tell "no such symbol" (0) apart
/// from "found them, none rewritable" (>0 with everything in `skipped`). A
/// rewriter that reported those two the same way would let a stale graph read
/// as a clean no-op.
fn collect_sites(
    refs: &[ScipRefRecord],
    symbol: &str,
    scope_prefix: Option<&str>,
    source_root: &Path,
) -> (Vec<Site>, Skipped, usize) {
    let mut sites = Vec::new();
    let mut skipped = Skipped::default();
    let mut considered = 0usize;
    let mut cache: BTreeMap<String, Option<Vec<String>>> = BTreeMap::new();

    for r in refs {
        if r.callee_symbol != symbol {
            continue;
        }
        if let Some(prefix) = scope_prefix {
            if !r.file_path.starts_with(prefix) {
                continue;
            }
        }
        considered += 1;

        // Refusal 3 — a row with no span is not a site.
        if !r.has_span() {
            skipped.no_span += 1;
            continue;
        }
        if r.end_line != r.line {
            skipped.multiline += 1;
            continue;
        }

        let lines = cache.entry(r.file_path.clone()).or_insert_with(|| {
            std::fs::read_to_string(source_root.join(&r.file_path))
                .ok()
                .map(|c| c.lines().map(str::to_string).collect())
        });
        let Some(lines) = lines.as_ref() else {
            skipped.out_of_range.push(r.file_path.clone());
            continue;
        };
        let Some(line) = lines.get(r.line as usize) else {
            skipped
                .out_of_range
                .push(format!("{}:{}", r.file_path, r.line + 1));
            continue;
        };

        // Refusal 2 — the span must still say what the graph claims.
        match slice_chars(line, r.start_col as usize, r.end_col as usize) {
            Some(found) if found == symbol => sites.push(Site {
                file: r.file_path.clone(),
                line: r.line,
                start_col: r.start_col,
                end_col: r.end_col,
            }),
            Some(found) => skipped.mismatch.push(format!(
                "{}:{} — span holds `{found}`, not `{symbol}`",
                r.file_path,
                r.line + 1
            )),
            None => skipped
                .out_of_range
                .push(format!("{}:{}", r.file_path, r.line + 1)),
        }
    }
    (sites, skipped, considered)
}

fn group_by_file(sites: Vec<Site>) -> BTreeMap<String, Vec<Site>> {
    let mut out: BTreeMap<String, Vec<Site>> = BTreeMap::new();
    for s in sites {
        out.entry(s.file.clone()).or_default().push(s);
    }
    out
}

fn report_skips(s: &Skipped, corpus_id: &str) {
    if s.no_span > 0 {
        println!(
            "\n  {} reference(s) carry NO SPAN — indexed before span recording. \
             They are not rewritable and were not touched.\n    \
             re-index: svrn project refresh --name {corpus_id} --local",
            s.no_span
        );
    }
    if !s.mismatch.is_empty() {
        println!(
            "\n  {} site(s) SKIPPED — the graph and the file disagree:",
            s.mismatch.len()
        );
        for m in s.mismatch.iter().take(5) {
            println!("    {m}");
        }
        if s.mismatch.len() > 5 {
            println!("    … and {} more", s.mismatch.len() - 5);
        }
    }
    if !s.out_of_range.is_empty() {
        println!(
            "\n  {} site(s) SKIPPED — file missing or span past end of file.",
            s.out_of_range.len()
        );
    }
    if s.multiline > 0 {
        println!(
            "\n  {} occurrence(s) span multiple lines and were not rewritten.",
            s.multiline
        );
    }
}

/// Byte offset of the `n`th char, or `None` when the line is shorter.
/// SCIP columns are character offsets; Rust slicing is byte-based, and a
/// non-ASCII line makes those differ. Indexing bytes with a char offset would
/// either panic or splice mid-codepoint.
fn char_to_byte(line: &str, n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    line.char_indices()
        .nth(n)
        .map(|(b, _)| b)
        .or(if line.chars().count() == n {
            Some(line.len())
        } else {
            None
        })
}

fn slice_chars(line: &str, start: usize, end: usize) -> Option<&str> {
    if end < start {
        return None;
    }
    let s = char_to_byte(line, start)?;
    let e = char_to_byte(line, end)?;
    line.get(s..e)
}

fn is_plain_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn registered_root(corpus_id: &str) -> Option<String> {
    let raw =
        std::fs::read_to_string(sovereign_cli_shared::dirs::sovereign_root().join("projects.json"))
            .ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.as_array()?
        .iter()
        .find(|p| p.get("corpus_id").and_then(|c| c.as_str()) == Some(corpus_id))
        .and_then(|p| p.get("root")?.as_str().map(str::to_string))
}

fn resolve_corpus(explicit: Option<String>, indexes_dir: &Path) -> Result<String, i32> {
    if let Some(c) = explicit {
        return Ok(c);
    }
    let mut corpora: Vec<String> = std::fs::read_dir(indexes_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("scip_graph.db").exists())
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    corpora.sort();
    match corpora.len() {
        1 => Ok(corpora.remove(0)),
        0 => {
            eprintln!(
                "error: no code corpus under {} — run `svrn project init` first",
                indexes_dir.display()
            );
            Err(ERROR)
        }
        _ => {
            eprintln!(
                "error: multiple code corpora — pass --corpus-id <one of: {}>",
                corpora.join(", ")
            );
            Err(ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(file: &str, callee: &str, line: i32, sc: i32, ec: i32) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: "caller".into(),
            callee_symbol: callee.into(),
            caller_qualified: "c".into(),
            callee_qualified: "x".into(),
            file_path: file.into(),
            line,
            start_col: sc,
            end_line: line,
            end_col: ec,
            ref_kind: "direct".into(),
        }
    }

    fn fixture(body: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let name = "src.rs";
        std::fs::write(dir.path().join(name), body).unwrap();
        (dir, name.to_string())
    }

    #[test]
    fn rewrites_only_the_identifier_span() {
        let (dir, f) = fixture("let x = strip_html(input);\n");
        // `strip_html` starts at char 8, ends at 18.
        let refs = vec![r(&f, "strip_html", 0, 8, 18)];
        let (sites, skipped, considered) =
            collect_sites(&refs, "strip_html", None, dir.path());
        assert_eq!(considered, 1);
        assert_eq!(sites.len(), 1);
        assert_eq!(skipped.mismatch.len(), 0);
    }

    /// The refusal that keeps a stale graph from corrupting source: the span
    /// still resolves inside the file, but the text under it is not the symbol.
    #[test]
    fn refuses_a_span_whose_text_moved() {
        let (dir, f) = fixture("let x = something_else(input);\n");
        let refs = vec![r(&f, "strip_html", 0, 8, 18)];
        let (sites, skipped, considered) =
            collect_sites(&refs, "strip_html", None, dir.path());
        assert_eq!(considered, 1, "the reference was counted");
        assert!(sites.is_empty(), "but it must not be rewritten");
        assert_eq!(skipped.mismatch.len(), 1);
    }

    /// A pre-migration row reads -1 and must never be treated as column 0.
    #[test]
    fn refuses_a_row_with_no_span() {
        let (dir, f) = fixture("let x = strip_html(input);\n");
        let mut rec = r(&f, "strip_html", 0, -1, -1);
        rec.end_line = -1;
        let (sites, skipped, considered) =
            collect_sites(&[rec], "strip_html", None, dir.path());
        assert_eq!(considered, 1);
        assert!(sites.is_empty());
        assert_eq!(skipped.no_span, 1);
    }

    #[test]
    fn unknown_symbol_is_zero_considered_not_a_clean_pass() {
        let (dir, f) = fixture("let x = strip_html(input);\n");
        let refs = vec![r(&f, "strip_html", 0, 8, 18)];
        let (_, _, considered) = collect_sites(&refs, "no_such_fn", None, dir.path());
        assert_eq!(considered, 0);
    }

    #[test]
    fn scope_prefix_limits_the_sweep() {
        let (dir, _) = fixture("let x = strip_html(input);\n");
        let refs = vec![r("src.rs", "strip_html", 0, 8, 18)];
        let (_, _, considered) =
            collect_sites(&refs, "strip_html", Some("other/"), dir.path());
        assert_eq!(considered, 0);
    }

    #[test]
    fn char_offsets_survive_non_ascii_lines() {
        // `é` is two bytes; a byte-indexed slice would splice mid-codepoint.
        let line = "let é = strip_html(x);";
        assert_eq!(slice_chars(line, 8, 18), Some("strip_html"));
    }

    #[test]
    fn rejects_a_non_identifier_replacement() {
        assert!(!is_plain_ident("foo::bar"));
        assert!(!is_plain_ident(""));
        assert!(is_plain_ident("strip_html"));
        assert!(is_plain_ident("_x2"));
    }
}
