// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich backfill-sections` — fill the chunk → section join in a
//! corpus's `chapters.json`.
//!
//! # What this repairs
//!
//! `ChapterEntry::chunk_ids` is the bridge between LanceDB row ids (what
//! retrieval carries on a `ScoredChunk`) and section ids (what the rest of
//! the system cites, and what `governance_view::section_titles` turns into a
//! human heading like `CHAPTER VII`). Two production readers already depend
//! on it. It was never written: `ChapterManifest::from_detected_sections`
//! sets it empty, and the enrich call site's own comment deferred the rest to
//! "a future LanceDB ingest".
//!
//! Measured 2026-08-05: 9 of 1788 local corpora had a populated join, all of
//! them from the `--from-corpus` path where chapters ARE chunks. Every other
//! corpus had an empty one, so every reader silently got an empty map and
//! behaved as though the corpus had no structure at all.
//!
//! # The two corpus layouts, and why the second one is the important one
//!
//! **Self-indexed** — the corpus owns its `chunks.lance`. 38 of 1825 local
//! index dirs. `chaos-saltgrass` and every single-document bench corpus.
//!
//! **Sibling** — the index dir holds `atlas/` + `chapters.json` and NO
//! chunks, because the text lives in a combined parent corpus. ~1787 of 1825,
//! and it is the layout of everything published to Hugging Face. SEP is 1771
//! sibling dirs whose sections are all empty, while all 187,967 chunks sit in
//! one `sep/chunks.lance` that has no `chapters.json` at all — the two halves
//! of the join are in different directories.
//!
//! A sibling is resolved from its own config: `sep-abduction`'s source is
//! `…/corpora/sep/articles/abduction.md`, which names both the parent (`sep`)
//! and the document key (`abduction`). Parent chunks carry `title:
//! "abduction"`, so the slice is selectable without parsing corpus ids.
//!
//! # Publish, not download
//!
//! A downloader has `chapters.json` and no source document, so they cannot
//! run this. The join has to be computed on the machine that has the sources,
//! BEFORE a bundle is published, or every downstream user inherits a corpus
//! whose citations can never name a section.
//!
//! # Absence is reported, never defaulted
//!
//! The arithmetic is always printed, never just a success line: a partial
//! join and a complete one are different claims and the readers downstream
//! cannot tell them apart (ARCH_PRINCIPLES §18.3). Exit 3 means "written, but
//! some chunks could not be placed".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::pipeline::{assign_chunks_to_sections, ChapterManifest};
use corpus_engine::EnrichmentChunkRow;
use sovereign_cli_shared::dirs::sovereign_root;

use super::config::EnrichConfig;
use super::corpus_io::{detector_for, fetch_all_corpus_chunks};
use super::paths;
use super::source_loader::load_plaintext;

/// Where a corpus's chunks come from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ChunkSource {
    /// The corpus owns its own `chunks.lance`.
    SelfIndexed,
    /// The chunks live in `parent`, and this corpus is the slice keyed by
    /// `doc_key` (matched against a chunk's title or source_doc_id).
    Parent { parent: String, doc_key: String },
}

struct Target {
    corpus_id: String,
    cfg: EnrichConfig,
    source: ChunkSource,
}

/// One corpus's outcome, for the `--all` summary. Every field is printed:
/// a sweep that quietly skipped 400 corpora is indistinguishable from one
/// that repaired them unless it says so.
struct Outcome {
    corpus_id: String,
    sections: usize,
    filled: usize,
    mapped: usize,
    unmapped: usize,
    /// `Some` = nothing was attempted, and why.
    skipped: Option<String>,
}

pub async fn cmd_backfill_sections(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut all = false;
    let mut dry_run = false;
    let mut chunks_from: Option<String> = None;
    let mut doc_key: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |flag: &str| match it.next() {
            Some(v) => Some(v.clone()),
            None => {
                eprintln!("error: {flag} needs a value");
                None
            }
        };
        match a.as_str() {
            "--all" => all = true,
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                usage();
                return 0;
            }
            "--chunks-from" => match val("--chunks-from") {
                Some(v) => chunks_from = Some(v),
                None => return 2,
            },
            "--doc-key" => match val("--doc-key") {
                Some(v) => doc_key = Some(v),
                None => return 2,
            },
            "--corpus" => match val("--corpus") {
                Some(v) => corpus_id = Some(v),
                None => return 2,
            },
            other if !other.starts_with('-') && corpus_id.is_none() => {
                corpus_id = Some(other.to_string())
            }
            other => {
                eprintln!("error: unrecognised argument `{other}`");
                usage();
                return 2;
            }
        }
    }
    if all && corpus_id.is_some() {
        eprintln!("error: --all and a corpus id are mutually exclusive");
        return 2;
    }
    if all && (chunks_from.is_some() || doc_key.is_some()) {
        eprintln!(
            "error: --chunks-from/--doc-key are per-corpus overrides and cannot be combined \
             with --all, which resolves each corpus from its own config."
        );
        return 2;
    }

    let targets = if all {
        match resolve_all() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        let Some(id) = corpus_id else {
            eprintln!("error: no corpus id (or --all)");
            usage();
            return 2;
        };
        match resolve_one(&id, chunks_from, doc_key) {
            Ok(t) => vec![t],
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    };

    run(targets, dry_run, all)
}

/// Group targets by chunk source so a parent's table is read ONCE, not once
/// per sibling. Reading `sep`'s 187,967 chunks for each of 1771 siblings is
/// the difference between a sweep that finishes and one that doesn't.
fn run(targets: Vec<Target>, dry_run: bool, sweep: bool) -> i32 {
    let mut by_source: BTreeMap<String, Vec<Target>> = BTreeMap::new();
    for t in targets {
        let key = match &t.source {
            ChunkSource::SelfIndexed => t.corpus_id.clone(),
            ChunkSource::Parent { parent, .. } => parent.clone(),
        };
        by_source.entry(key).or_default().push(t);
    }

    let mut outcomes: Vec<Outcome> = Vec::new();
    for (chunk_corpus, group) in by_source {
        if sweep {
            eprintln!(
                "[backfill] reading chunks of `{chunk_corpus}` for {} corpus/corpora…",
                group.len()
            );
        }
        let chunks = match fetch_all_corpus_chunks(&chunk_corpus) {
            Ok(c) => c,
            Err(e) => {
                for t in group {
                    outcomes.push(Outcome {
                        corpus_id: t.corpus_id,
                        sections: 0,
                        filled: 0,
                        mapped: 0,
                        unmapped: 0,
                        skipped: Some(format!("chunks of `{chunk_corpus}` unreadable: {e}")),
                    });
                }
                continue;
            }
        };
        for t in group {
            outcomes.push(backfill_one(&t, &chunks, dry_run, sweep));
        }
    }

    if sweep {
        print_sweep(&outcomes);
    }
    let attempted = outcomes.iter().filter(|o| o.skipped.is_none()).count();
    let incomplete = outcomes
        .iter()
        .any(|o| o.skipped.is_none() && o.unmapped > 0);
    if attempted == 0 {
        // Nothing ran. That is never a success — a sweep that repaired
        // nothing must not exit 0 and read as "all corpora are fine".
        eprintln!("error: no corpus was backfilled.");
        return 1;
    }
    if incomplete {
        3
    } else {
        0
    }
}

fn backfill_one(t: &Target, chunks: &[EnrichmentChunkRow], dry_run: bool, quiet: bool) -> Outcome {
    let skip = |why: String| Outcome {
        corpus_id: t.corpus_id.clone(),
        sections: 0,
        filled: 0,
        mapped: 0,
        unmapped: 0,
        skipped: Some(why),
    };
    let manifest_path = paths::chapters_manifest_path(&t.corpus_id);
    let mut manifest = match ChapterManifest::load(&manifest_path) {
        Ok(Some(m)) => m,
        Ok(None) => return skip("no chapter manifest (run `svrn enrich init` first)".into()),
        Err(e) => return skip(format!("manifest unreadable: {e}")),
    };
    let source = match load_plaintext(&t.cfg.source_path) {
        Ok(s) => s,
        Err(e) => {
            return skip(format!(
                "source {} unreadable: {e}",
                t.cfg.source_path.display()
            ))
        }
    };
    let detector = match detector_for(&t.cfg) {
        Ok(d) => d,
        Err(e) => return skip(format!("section detector: {e}")),
    };
    let sections = corpus_engine::chunkers::sectioned::SectionedChunker::with_detector(detector)
        .dry_run(&source)
        .sections;
    if sections.is_empty() {
        return skip(format!(
            "no sections detected in {} — the manifest's {} chapter(s) came from a different \
             detector configuration",
            t.cfg.source_path.display(),
            manifest.chapters.len()
        ));
    }

    // Narrow the parent's table to THIS document before locating anything.
    // Without it a sibling would be matched against every other article's
    // text, which is both wrong and quadratic.
    let selected: Vec<(u64, &str)> = match &t.source {
        ChunkSource::SelfIndexed => chunks.iter().map(|c| (c.id, c.content.as_str())).collect(),
        ChunkSource::Parent { doc_key, .. } => chunks
            .iter()
            .filter(|c| belongs_to_doc(c, doc_key))
            .map(|c| (c.id, c.content.as_str()))
            .collect(),
    };
    if selected.is_empty() {
        return skip(match &t.source {
            ChunkSource::SelfIndexed => "corpus has no chunks".into(),
            ChunkSource::Parent { parent, doc_key } => format!(
                "no chunk in `{parent}` matched doc key `{doc_key}` — the key is taken from the \
                 source filename and matched against a chunk's title or source_doc_id; override \
                 with --doc-key"
            ),
        });
    }

    let join = assign_chunks_to_sections(&source, &sections, &selected);
    let mut filled = 0usize;
    for entry in &mut manifest.chapters {
        if let Some(ids) = join.by_section.get(&entry.id) {
            entry.chunk_ids = ids.clone();
            filled += 1;
        }
        // A section no chunk landed in keeps whatever it had: this command
        // FILLS the join, and an empty section is a fact about chunking, not
        // a reason to destroy prior state.
    }

    if !quiet {
        println!("backfill-sections — {}", t.corpus_id);
        println!("  source:    {}", t.cfg.source_path.display());
        println!("  manifest:  {}", manifest_path.display());
        match &t.source {
            ChunkSource::SelfIndexed => println!("  chunks:    own index"),
            ChunkSource::Parent { parent, doc_key } => println!(
                "  chunks:    from `{parent}`, doc key `{doc_key}` ({} of {} rows selected)",
                selected.len(),
                chunks.len()
            ),
        }
        println!(
            "  sections:  {} detected, {} in manifest",
            sections.len(),
            manifest.chapters.len()
        );
        println!(
            "  chunks:    {} considered, {} mapped, {} UNMAPPED",
            selected.len(),
            join.mapped_chunks(),
            join.unmapped.len()
        );
        if !join.unmapped.is_empty() {
            let shown: Vec<String> = join.unmapped.iter().take(20).map(u64::to_string).collect();
            println!(
                "  unmapped ids: {}{}",
                shown.join(", "),
                if join.unmapped.len() > 20 { ", …" } else { "" }
            );
            println!(
                "  (unmapped = not findable in the source, findable in more than one place, or \
                 overlapping no section body. Never guessed at.)"
            );
        }
        println!("  filled:    {filled}/{} manifest section(s)", manifest.chapters.len());
    }

    if !dry_run {
        if let Err(e) = manifest.save(&manifest_path) {
            return skip(format!("writing manifest: {e}"));
        }
        if !quiet {
            println!("  written.");
        }
    } else if !quiet {
        println!("  DRY RUN — manifest not written.");
    }

    Outcome {
        corpus_id: t.corpus_id.clone(),
        sections: manifest.chapters.len(),
        filled,
        mapped: join.mapped_chunks(),
        unmapped: join.unmapped.len(),
        skipped: None,
    }
}

/// Does this parent-corpus chunk belong to the document `doc_key` names?
///
/// Title first (SEP stores the article slug there verbatim), then
/// `source_doc_id` as a path/URL segment. Both are exact-segment matches, not
/// substring containment — `abduction` must not sweep in `abduction-logic`.
fn belongs_to_doc(c: &EnrichmentChunkRow, doc_key: &str) -> bool {
    if c.title
        .as_deref()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case(doc_key))
    {
        return true;
    }
    c.source_doc_id.as_deref().is_some_and(|d| {
        d.trim_end_matches('/')
            .rsplit('/')
            .next()
            .is_some_and(|seg| seg.eq_ignore_ascii_case(doc_key))
    })
}

fn resolve_one(
    corpus_id: &str,
    chunks_from: Option<String>,
    doc_key: Option<String>,
) -> Result<Target, String> {
    let cfg = match EnrichConfig::load(corpus_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err(format!(
                "no enrichment config for `{corpus_id}` at {}.\nThe join needs the SAME section \
                 detector the manifest was built with; inferring one would silently produce a \
                 different set of sections.",
                paths::config_path(corpus_id).display()
            ))
        }
        Err(e) => return Err(format!("loading config for `{corpus_id}`: {e}")),
    };
    let source = resolve_source(corpus_id, &cfg, chunks_from, doc_key)?;
    Ok(Target { corpus_id: corpus_id.to_string(), cfg, source })
}

/// Explicit flags win; otherwise a corpus with its own `chunks.lance` is
/// self-indexed, and one whose source sits under `…/corpora/<parent>/…` is a
/// slice of `<parent>` keyed by the source filename stem.
fn resolve_source(
    corpus_id: &str,
    cfg: &EnrichConfig,
    chunks_from: Option<String>,
    doc_key: Option<String>,
) -> Result<ChunkSource, String> {
    let key_from_path = || {
        cfg.source_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
    };
    if let Some(parent) = chunks_from {
        let doc_key = doc_key.or_else(key_from_path).ok_or_else(|| {
            format!("`{corpus_id}`: --chunks-from given but no --doc-key and no source filename")
        })?;
        return Ok(ChunkSource::Parent { parent, doc_key });
    }
    if paths::index_root(corpus_id).join("chunks.lance").exists() {
        return Ok(ChunkSource::SelfIndexed);
    }
    match (infer_parent(&cfg.source_path), doc_key.or_else(key_from_path)) {
        (Some(parent), Some(doc_key)) => Ok(ChunkSource::Parent { parent, doc_key }),
        _ => Err(format!(
            "`{corpus_id}` has no chunks.lance of its own and no parent corpus could be inferred \
             from its source {}. Name one with --chunks-from <parent> [--doc-key <key>].",
            cfg.source_path.display()
        )),
    }
}

/// `~/.svrnmesh/corpora/<parent>/articles/abduction.md` → `<parent>`.
///
/// Tries the textual prefix first, then both sides canonicalized. The second
/// attempt is not belt-and-braces: on this host `~/.sovereign` is a SYMLINK
/// to `~/.svrnmesh`, configs store the symlink path, and `sovereign_root()`
/// returns the real one — so a purely textual `strip_prefix` fails on every
/// SEP sibling. Canonicalising only the root, or only the source, would
/// leave the mirror-image deployment broken instead.
fn infer_parent(source_path: &Path) -> Option<String> {
    let root = sovereign_root().join("corpora");
    infer_parent_under(&root, source_path).or_else(|| {
        let real_root = root.canonicalize().ok()?;
        let real_source = source_path.canonicalize().ok()?;
        infer_parent_under(&real_root, &real_source)
    })
}

/// Pure half: the first path component under `corpora_root`. Anchored on the
/// root rather than a positional index so a deeper or shallower layout under
/// it still resolves.
fn infer_parent_under(corpora_root: &Path, source_path: &Path) -> Option<String> {
    let rest = source_path.strip_prefix(corpora_root).ok()?;
    let parent = rest.components().next()?;
    // A file sitting directly in `corpora/` has no parent corpus — its first
    // component IS the file, and claiming it as a corpus id would send the
    // read at a table that cannot exist.
    if rest.components().count() < 2 {
        return None;
    }
    Some(parent.as_os_str().to_string_lossy().into_owned())
}

/// Every corpus that has an enrichment config AND a chapter manifest. A
/// corpus with no manifest has no sections to join and is not a candidate.
fn resolve_all() -> Result<Vec<Target>, String> {
    let root = sovereign_root().join("enrichment");
    let entries = std::fs::read_dir(&root)
        .map_err(|e| format!("reading {}: {e}", root.display()))?;
    let mut ids: Vec<String> = Vec::new();
    for e in entries.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        if paths::chapters_manifest_path(&id).exists() {
            ids.push(id);
        }
    }
    ids.sort();
    let total = ids.len();
    let mut targets = Vec::new();
    let mut unresolved = 0usize;
    for id in ids {
        match resolve_one(&id, None, None) {
            Ok(t) => targets.push(t),
            Err(_) => unresolved += 1,
        }
    }
    // Reported, not swallowed: "1400 of 1771 repaired" and "1771 repaired"
    // are different claims about the fleet.
    eprintln!(
        "[backfill] {total} corpus/corpora with a chapter manifest; {} resolvable, {unresolved} \
         could not be resolved to a chunk source.",
        targets.len()
    );
    Ok(targets)
}

fn print_sweep(outcomes: &[Outcome]) {
    let done: Vec<&Outcome> = outcomes.iter().filter(|o| o.skipped.is_none()).collect();
    let skipped: Vec<&Outcome> = outcomes.iter().filter(|o| o.skipped.is_some()).collect();
    println!();
    println!("backfill-sections — sweep");
    println!("  corpora backfilled: {}", done.len());
    println!(
        "  sections filled:    {} of {}",
        done.iter().map(|o| o.filled).sum::<usize>(),
        done.iter().map(|o| o.sections).sum::<usize>()
    );
    println!(
        "  chunks mapped:      {} ({} unmapped)",
        done.iter().map(|o| o.mapped).sum::<usize>(),
        done.iter().map(|o| o.unmapped).sum::<usize>()
    );
    let fully_unmapped: Vec<&&Outcome> = done.iter().filter(|o| o.mapped == 0).collect();
    if !fully_unmapped.is_empty() {
        println!(
            "  {} corpus/corpora mapped NOTHING (join still empty): {}",
            fully_unmapped.len(),
            fully_unmapped
                .iter()
                .take(10)
                .map(|o| o.corpus_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !skipped.is_empty() {
        println!("  skipped: {}", skipped.len());
        let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
        for o in &skipped {
            let why = o.skipped.as_deref().unwrap_or("");
            // Group by the reason's head so the tail (a path, an id) doesn't
            // fragment the tally into one bucket per corpus.
            let head = why.split(&['—', ':'][..]).next().unwrap_or(why).trim();
            *reasons.entry(head).or_default() += 1;
        }
        for (why, n) in reasons {
            println!("    {n:>5}  {why}");
        }
    }
}

fn usage() {
    eprintln!("usage: svrn enrich backfill-sections <corpus-id> [--chunks-from <parent>]");
    eprintln!("                                     [--doc-key <key>] [--dry-run]");
    eprintln!("       svrn enrich backfill-sections --all [--dry-run]");
    eprintln!();
    eprintln!("  Fill chapters.json's per-section chunk_ids by locating each stored chunk in");
    eprintln!("  the source document and assigning it to the section whose body contains it.");
    eprintln!();
    eprintln!("  Two layouts, resolved automatically:");
    eprintln!("    self-indexed  the corpus has its own chunks.lance");
    eprintln!("    sibling       chunks live in a parent corpus; the parent and the document");
    eprintln!("                  key are inferred from the source path");
    eprintln!("                  (…/corpora/<parent>/…/<key>.md). Override with the flags.");
    eprintln!();
    eprintln!("  --all sweeps every corpus that has a chapter manifest, reading each parent's");
    eprintln!("  chunk table ONCE.");
    eprintln!();
    eprintln!("  Run this BEFORE publishing a bundle: a downloader has chapters.json and no");
    eprintln!("  source document, so they cannot repair the join themselves.");
    eprintln!();
    eprintln!("  Exit: 0 = every chunk mapped · 3 = written, some chunks unmapped · 1 = error");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, title: Option<&str>, doc: Option<&str>) -> EnrichmentChunkRow {
        EnrichmentChunkRow {
            id,
            content: String::new(),
            title: title.map(str::to_string),
            url: None,
            metadata_raw: None,
            source_doc_id: doc.map(str::to_string),
        }
    }

    #[test]
    fn a_doc_key_matches_the_title_verbatim() {
        assert!(belongs_to_doc(&row(1, Some("abduction"), None), "abduction"));
        assert!(belongs_to_doc(&row(1, Some("Abduction"), None), "abduction"));
    }

    /// The SEP shape: title carries the slug, source_doc_id carries the URL.
    #[test]
    fn a_doc_key_matches_the_last_url_segment() {
        let r = row(1, None, Some("https://plato.stanford.edu/entries/abduction/"));
        assert!(belongs_to_doc(&r, "abduction"));
    }

    /// The bug an eager substring match would ship: one article's chunks
    /// swept into a neighbour whose slug merely starts the same way.
    #[test]
    fn a_doc_key_does_not_match_a_longer_neighbour() {
        assert!(!belongs_to_doc(&row(1, Some("abduction-logic"), None), "abduction"));
        let r = row(2, None, Some("https://plato.stanford.edu/entries/abduction-logic/"));
        assert!(!belongs_to_doc(&r, "abduction"));
    }

    #[test]
    fn a_chunk_naming_no_document_belongs_to_none() {
        assert!(!belongs_to_doc(&row(1, None, None), "abduction"));
    }

    #[test]
    fn the_parent_is_the_first_component_under_corpora() {
        let root = Path::new("/home/u/.sovereign/corpora");
        let p = Path::new("/home/u/.sovereign/corpora/sep/articles/abduction.md");
        assert_eq!(infer_parent_under(root, p).as_deref(), Some("sep"));
    }

    /// Layout depth must not matter — only the anchor.
    #[test]
    fn a_deeper_layout_still_resolves_to_the_first_component() {
        let root = Path::new("/r/corpora");
        let p = Path::new("/r/corpora/sep/a/b/c/abduction.md");
        assert_eq!(infer_parent_under(root, p).as_deref(), Some("sep"));
    }

    /// A file directly in `corpora/` is not inside a corpus directory, so
    /// there is no parent — claiming one would point the chunk read at a
    /// corpus id that is really a filename.
    #[test]
    fn a_file_directly_under_corpora_has_no_parent() {
        let root = Path::new("/r/corpora");
        assert_eq!(infer_parent_under(root, Path::new("/r/corpora/book.txt")), None);
    }

    #[test]
    fn a_source_outside_corpora_infers_no_parent() {
        let root = Path::new("/r/corpora");
        assert_eq!(infer_parent_under(root, Path::new("/tmp/elsewhere/book.txt")), None);
        // And the real accessor agrees for a path that exists nowhere near it.
        assert_eq!(infer_parent(Path::new("/tmp/elsewhere/book.txt")), None);
    }
}
