// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich atlas-patch-code <atlas-corpus>` — Inc 6, first-class
//! patchability for a CODE atlas.
//!
//! When N functions change, re-derive ONLY those functions' atoms + edges and
//! patch the v2 atlas incrementally — never a full rebuild. The code analogue
//! of `enrich delta` (which patches a referential/LLM atlas): it refreshes the
//! source corpus's code-intel cache (body-hash-gated, so only changed bodies
//! cost a model call), diffs the prior vs refreshed cache to find the changed
//! / removed symbols, re-derives just those atoms via
//! `code_walk::extract_atoms_for_symbols`, merges them with `apply_atom_delta`,
//! and **rebuilds the v2 store (atoms.lance + edges.csr)** so a `.read_v2`
//! corpus never serves a stale atom. ANN seeds are refreshed last.
//!
//! ## Flow
//!
//!   1. Resolve the atlas corpus; pre-flight its atoms carry content-hash ids
//!      (`apply_atom_delta` would orphan sequential-id atoms).
//!   2. Resolve the SOURCE code corpus: `--source-corpus`, else the atlas's
//!      `schema_validation.json::source_corpus_id`, else the
//!      `<source>-self-atlas` / `<source>-atlas` naming convention. It must
//!      carry a `scip_graph.db` (it's a code corpus).
//!   3. Snapshot the prior code-intel cache → refresh it via the code-intel
//!      pass (cache-only; `SOVEREIGN_ENRICH_SKIP_INDEX`) → re-read it.
//!   4. `diff_code_intel_caches(prior, refreshed)` → changed + removed
//!      doc-anchors (== qualified-names == atlas item doc-ids).
//!   5. `extract_atoms_for_symbols` → an additive/upsert `AtomsDelta`.
//!   6. Back up the atlas json → ensure the doc→atoms sidecar →
//!      `apply_atom_delta`.
//!   7. `build_and_write_store` — THE load-bearing v2 rebuild.
//!   8. `backfill-ann` with the code-aware filter flags (best-effort).
//!
//! KNOWN LIMITS: salience (a global in-degree statistic) is NOT recomputed on
//! a patch; incoming `ScipStructural` edges (unchanged-caller → changed-symbol)
//! are dropped and refresh on the next full rebuild. See the module note on
//! `code_walk::extract_atoms_for_symbols`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms_delta::apply_atom_delta;
use corpus_engine::enrichment::atlas::store::build_and_write_store;
use corpus_engine::enrichment::atlas::strategies::code_walk::{
    extract_atoms_for_symbols, read_code_walk_visibility, CodeWalkConfig,
};
use corpus_engine::enrichment::atlas::{doc_to_atoms, read_atlas_atoms, ATLAS_DIRNAME};
use corpus_engine::enrichment::code_intel::pass::run_code_intel_for_corpus;
use corpus_engine::enrichment::code_intel::{diff_code_intel_caches, SymbolEnrichment};
use corpus_engine::{CorpusEngine, EmbedFn};

use super::inference_client::{probe_daemon, DaemonInferenceClient};
use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich atlas-patch-code",
    summary: "Incrementally patch a CODE atlas for the functions that changed (no full rebuild).",
    sections: &[
        HelpSection::Usage(
            "svrn enrich atlas-patch-code <atlas-corpus> \\\n  [--source-corpus <id>] [--files a.rs,b.rs] [--include-functions] [--include-private]",
        ),
        HelpSection::Flags(&[
            (
                "<atlas-corpus>",
                "The CODE atlas corpus to patch (id, name, or unique substring), e.g. \
                 semver-self-atlas. Must have a v2 store; sequential-id atoms are rejected.",
            ),
            (
                "--source-corpus <id>",
                "The indexed source code corpus the atlas was built from. Default: the \
                 atlas's recorded source, else the <source>-self-atlas / <source>-atlas \
                 naming convention.",
            ),
            (
                "--files a.rs,b.rs",
                "Scope the code-intel refresh to symbols whose file path contains one of \
                 these substrings. Empty = whole corpus (body-hash gating still means only \
                 changed bodies cost a model call).",
            ),
            (
                "--include-functions / --include-private",
                "Override the visibility config used to re-derive atoms. Default: recovered \
                 from the atlas (its schema_validation, else inferred from its atoms) so the \
                 patch reproduces the original item universe.",
            ),
        ]),
        HelpSection::Examples(&[(
            "svrn enrich atlas-patch-code semver-self-atlas",
            "Refresh semver's code-intel summaries, patch the changed functions' atoms+edges, rebuild the v2 store.",
        )]),
        HelpSection::Notes(
            "Additive/upsert only: the atlas json is backed up to \
             <atlas>/.patch-backup/ and mutated via apply_atom_delta — never rebuilt. \
             Requires the daemon for the code-intel summary refresh. The v2 store \
             (atoms.lance + edges.csr) IS rebuilt so a .read_v2 corpus never reads stale.",
        ),
    ],
};

pub async fn cmd_atlas_patch_code(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }

    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        });
    let indexes_dir = data_dir.join("indexes");
    let recipes_dir = data_dir.join("recipes");

    // ── Step 1: pre-flight the atlas ───────────────────────────
    // The atlas corpus is addressed by directory name (like `atlas-query` /
    // `migrate-all`) — a code atlas built via `enrich ingest` has only an
    // `atlas/` dir and no registered `_corpus_meta.json`, so it isn't in the
    // installed-corpus list `resolve_corpus_id` searches.
    let atlas_id = parsed.atlas_corpus.clone();
    let atlas_dir = paths::index_root(&atlas_id).join(ATLAS_DIRNAME);
    if !atlas_dir.exists() {
        eprintln!(
            "error: no atlas at {}. Build it first (e.g. `svrn enrich ingest {atlas_id} --source-corpus <code> --include-functions`).",
            atlas_dir.display()
        );
        return 1;
    }
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading {}/atoms.json: {e}", atlas_dir.display());
            return 1;
        }
    };
    if !atoms_file.atoms.is_empty()
        && !atoms_file.atoms.iter().all(|env| env.id().is_content_hash())
    {
        eprintln!(
            "error: atlas `{atlas_id}` has sequential-id atoms; merging a content-hash delta \
             would orphan them.\n  Run `svrn atlas migrate-ids --corpus {atlas_id}` first."
        );
        return 1;
    }

    // ── Step 2: resolve the SOURCE code corpus ─────────────────
    let source_id = match resolve_source_corpus(&indexes_dir, &atlas_id, &atlas_dir, &parsed) {
        Ok(id) => id,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };
    let source_dir = indexes_dir.join(&source_id);
    if !source_dir.join("scip_graph.db").exists() {
        eprintln!(
            "error: source corpus `{source_id}` has no scip_graph.db — it isn't a code corpus. \
             Pass --source-corpus <id> to point at the right one."
        );
        return 1;
    }
    println!("  atlas         = {atlas_id}");
    println!("  source corpus = {source_id}");

    // Visibility: recover from the atlas unless overridden, so the re-derived
    // atoms reproduce the original item universe.
    let (inc_fn, inc_priv) = read_code_walk_visibility(&atlas_dir);
    let include_functions = parsed.include_functions.unwrap_or(inc_fn);
    let include_private = parsed.include_private.unwrap_or(inc_priv);
    println!("  include_functions = {include_functions} · include_private = {include_private}");

    // ── Step 3: refresh the code-intel cache (daemon) ──────────
    let cfg = match EnrichConfig::require(&source_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: loading enrichment config for `{source_id}`: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon not responding at {} — needed to refresh code-intel summaries.",
            cfg.base_url
        );
        return 2;
    }
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (embed, chat) = client.into_closures();

    // Snapshot prior cache BEFORE the refresh rewrites it.
    let prior = load_cache(&source_dir);

    // Cache-only refresh: we want the summaries, not a chunks.lance re-index
    // (the daemon may be co-managing it — cache-only is the conflict-free path).
    std::env::set_var("SOVEREIGN_ENRICH_SKIP_INDEX", "1");
    println!("  · refreshing code-intel summaries (body-hash gated; daemon) ...");
    match run_code_intel_for_corpus(&source_dir, &source_id, &chat, &embed, &parsed.files).await {
        Ok(report) => println!(
            "  ✓ code-intel: {} symbols | regenerated {} (reused {}, failed {})",
            report.symbols, report.enrich.regenerated, report.enrich.reused, report.enrich.failed,
        ),
        Err(e) => {
            eprintln!("error: code-intel refresh failed: {e}");
            return 1;
        }
    }
    let refreshed = load_cache(&source_dir);

    // ── Step 4: compute the change-set ─────────────────────────
    let change = diff_code_intel_caches(&prior, &refreshed);
    println!(
        "  · change-set: {} changed/new · {} removed (of {} cached symbols)",
        change.changed.len(),
        change.removed.len(),
        refreshed.len(),
    );
    if change.is_empty() {
        println!();
        println!("  ✓ no symbol bodies changed since the last summary — atlas already current (no-op).");
        return 0;
    }

    // ── Step 5: re-derive the scoped atoms + edges ─────────────
    let noop_embed: EmbedFn = Arc::new(|_| Box::pin(async { Ok(Vec::<f32>::new()) }));
    let engine = Arc::new(CorpusEngine::new(
        recipes_dir,
        indexes_dir.clone(),
        noop_embed,
    ));
    let walk_cfg = CodeWalkConfig {
        source_corpus_id: source_id.clone(),
        include_functions,
        include_private,
    };
    let delta = match extract_atoms_for_symbols(
        engine,
        &walk_cfg,
        &change.changed,
        &change.removed,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: re-deriving changed symbols' atoms: {e}");
            return 1;
        }
    };
    if delta.is_empty() {
        println!();
        println!(
            "  ✓ the change-set matched no atoms in this atlas (visibility filter excludes them) — no-op."
        );
        return 0;
    }

    // ── Step 6: back up → sidecar → apply ──────────────────────
    let backup_dir = atlas_dir.join(".patch-backup");
    if let Err(e) = backup_atlas_files(&atlas_dir, &backup_dir) {
        eprintln!("error: backing up atlas files to {}: {e}", backup_dir.display());
        return 1;
    }
    println!("  ✓ backed up atlas json → {}", backup_dir.display());

    // The ingest path doesn't write doc_to_atoms.json; build it so
    // removed_doc_ids can find atoms to drop + upserts replace cleanly.
    if let Err(e) = doc_to_atoms::build_and_write(&atlas_dir) {
        eprintln!("error: building doc_to_atoms.json sidecar: {e}");
        return 1;
    }

    let summary = match apply_atom_delta(&atlas_dir, delta) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: applying delta to {}: {e}\n  Backups are at {}.",
                atlas_dir.display(),
                backup_dir.display()
            );
            return 1;
        }
    };

    // ── Step 7: rebuild the v2 store (THE load-bearing step) ───
    println!("  · rebuilding v2 store (atoms.lance + edges.csr) ...");
    match build_and_write_store(&atlas_dir, &atlas_id).await {
        Ok(p) => println!("  ✓ rebuilt {}", p.display()),
        Err(e) => {
            eprintln!(
                "error: rebuilding v2 store: {e}\n  atoms.json/edges.json ARE patched; \
                 `svrn atlas migrate-all {atlas_id}` will rebuild the store."
            );
            return 1;
        }
    }

    // ── Step 8: refresh the ANN seed table (best-effort) ───────
    println!("  · refreshing ANN seed table (code-aware filter) ...");
    let ann_args = vec![
        atlas_id.clone(),
        "--atlas-depth".to_string(),
        "structural".to_string(),
        "--atlas-min-description-chars".to_string(),
        "1".to_string(),
    ];
    let ann_code = crate::atlas_cmd::backfill_ann::run(&ann_args).await;
    if ann_code != 0 {
        eprintln!(
            "  warning: backfill-ann exited {ann_code} — conceptual seeds may lag; named \
             queries + the v2 store are already current."
        );
    }

    // ── Summary ────────────────────────────────────────────────
    println!();
    println!("  ✓ patched {atlas_id}");
    println!(
        "      atoms: {} → {} (+{} new, -{} removed)",
        summary.atoms_before, summary.atoms_after, summary.atoms_added, summary.atoms_removed,
    );
    println!(
        "      docs upserted: {} · docs removed: {} · edges dropped: {}",
        summary.docs_upserted, summary.docs_removed, summary.edges_dropped,
    );
    println!("      files touched: {}", summary.files_touched.join(", "));
    0
}

/// Read a corpus's `code_intel_cache.json` as a `Vec<SymbolEnrichment>`.
/// Missing / unparseable → empty (a first-ever run has no prior).
fn load_cache(corpus_dir: &Path) -> Vec<SymbolEnrichment> {
    std::fs::read_to_string(corpus_dir.join("code_intel_cache.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Resolve the source code corpus: explicit flag → atlas's recorded
/// `source_corpus_id` → `<source>-self-atlas` / `<source>-atlas` naming
/// convention. The candidate is validated through `resolve_corpus_id` (it
/// must be installed).
fn resolve_source_corpus(
    indexes_dir: &Path,
    atlas_id: &str,
    atlas_dir: &Path,
    parsed: &ParsedPatch,
) -> Result<String, String> {
    if let Some(explicit) = &parsed.source_corpus {
        return crate::corpus_resolve::resolve_corpus_id(indexes_dir, explicit);
    }
    // Atlas's recorded source (written by the post-Inc-6 full build).
    if let Ok(s) = std::fs::read_to_string(atlas_dir.join("schema_validation.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            if let Some(src) = v
                .get("source_corpus_id")
                .and_then(|x| x.as_str())
                .filter(|x| !x.is_empty())
            {
                return crate::corpus_resolve::resolve_corpus_id(indexes_dir, src);
            }
        }
    }
    // Naming convention.
    let candidate = atlas_id
        .strip_suffix("-self-atlas")
        .or_else(|| atlas_id.strip_suffix("-atlas"))
        .ok_or_else(|| {
            format!(
                "could not infer the source corpus from atlas id `{atlas_id}` (no \
                 -self-atlas/-atlas suffix and no recorded source). Pass --source-corpus <id>."
            )
        })?;
    crate::corpus_resolve::resolve_corpus_id(indexes_dir, candidate).map_err(|e| {
        format!("naming-convention source `{candidate}` not installed ({e}). Pass --source-corpus <id>.")
    })
}

/// Copy the atlas's source-of-truth json into `backup_dir` (atoms / edges /
/// doc_to_atoms). The v2 store is rebuildable from these, so it isn't backed
/// up. Absent files are skipped.
fn backup_atlas_files(atlas_dir: &Path, backup_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(backup_dir)?;
    for name in ["atoms.json", "edges.json", "doc_to_atoms.json"] {
        let src = atlas_dir.join(name);
        if src.exists() {
            std::fs::copy(&src, backup_dir.join(name))?;
        }
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ParsedPatch {
    atlas_corpus: String,
    source_corpus: Option<String>,
    files: Vec<String>,
    include_functions: Option<bool>,
    include_private: Option<bool>,
}

fn parse_args(args: &[String]) -> Result<ParsedPatch, String> {
    let mut out = ParsedPatch::default();
    let mut atlas: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source-corpus" => {
                out.source_corpus = Some(
                    args.get(i + 1)
                        .ok_or_else(|| "--source-corpus requires a value".to_string())?
                        .clone(),
                );
                i += 2;
            }
            "--files" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--files requires a comma-separated list".to_string())?;
                out.files = split_csv(raw);
                i += 2;
            }
            other if other.strip_prefix("--files=").is_some() => {
                out.files = split_csv(other.strip_prefix("--files=").unwrap());
                i += 1;
            }
            "--include-functions" => {
                out.include_functions = Some(true);
                i += 1;
            }
            "--include-private" => {
                out.include_private = Some(true);
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if atlas.is_none() {
                    atlas = Some(other.to_string());
                    i += 1;
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
            }
        }
    }
    out.atlas_corpus = atlas.ok_or_else(|| "missing <atlas-corpus>".to_string())?;
    Ok(out)
}

fn split_csv(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_atlas_corpus() {
        assert!(parse_args(&[]).unwrap_err().contains("atlas-corpus"));
    }

    #[test]
    fn parse_happy_path_and_flags() {
        let p = parse_args(&[
            "semver-self-atlas".into(),
            "--source-corpus".into(),
            "semver".into(),
            "--files".into(),
            "lib.rs, parse.rs".into(),
            "--include-functions".into(),
        ])
        .unwrap();
        assert_eq!(p.atlas_corpus, "semver-self-atlas");
        assert_eq!(p.source_corpus.as_deref(), Some("semver"));
        assert_eq!(p.files, vec!["lib.rs", "parse.rs"]);
        assert_eq!(p.include_functions, Some(true));
        assert_eq!(p.include_private, None);
    }

    #[test]
    fn parse_files_eq_form() {
        let p = parse_args(&["a-self-atlas".into(), "--files=x.rs,y.rs".into()]).unwrap();
        assert_eq!(p.files, vec!["x.rs", "y.rs"]);
    }

    #[test]
    fn parse_rejects_unknown_flag() {
        assert!(parse_args(&["a".into(), "--nope".into()])
            .unwrap_err()
            .contains("unknown flag"));
    }

    #[test]
    fn source_convention_strips_self_atlas() {
        // Convention fallback strips -self-atlas before -atlas.
        assert_eq!("semver-self-atlas".strip_suffix("-self-atlas"), Some("semver"));
        assert_eq!(
            "commonwealth-ai-atlas"
                .strip_suffix("-self-atlas")
                .or_else(|| "commonwealth-ai-atlas".strip_suffix("-atlas")),
            Some("commonwealth-ai")
        );
    }
}
