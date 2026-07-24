// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn alignment` — operator-facing migration + status entry
//! point for the mesh-replicated alignment workspace recipe.
//!
//! ## What lands here
//!
//! - `alignment migrate [--dry-run]` — kick off the local alignment
//!   ingest (so this peer's partition is up to date) after taking a
//!   defensive backup at `~/.sovereign/backups/`. The actual cross-
//!   machine merge is handled by the existing daemon hooks
//!   (`auto_recover` / `index_transfer`); this command's job is to
//!   land the local side and give the operator visibility.
//! - `alignment status` — list what's in scope (markdown files +
//!   notes.db rows), report the local alignment corpus chunk count if
//!   it's been ingested, and surface anything anomalous (no daemon
//!   running, ~/.claude missing, etc.).
//!
//! Why a separate command rather than `svrn corpus install
//! alignment`: the alignment recipe is a sync transport, not a
//! browseable knowledge corpus; its operator workflow has a backup
//! step the regular install path doesn't, and `migrate --dry-run` is
//! a useful affordance that doesn't fit the corpus-install help
//! surface.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use corpus_engine::extractors::alignment_workspace::AlignmentWorkspaceExtractor;
use corpus_engine::extractors::Extractor;
use corpus_engine::IngestProgress;

const CORPUS_ID: &str = "alignment";

/// The canonical alignment recipe, owned inline by the command that
/// drives the alignment workflow.
///
/// It is deliberately NOT loaded from `sovereign-recipes/alignment/` —
/// that path is `.gitignore`d (the alignment corpus syncs the author's
/// private `~/.claude`, so nothing about it belongs in the tracked
/// recipe tree), which means an `include_str!` of it fails to compile on
/// any clean checkout. Embedding the TOML as a `const` keeps the recipe
/// out of the shared catalog while making the build self-contained: no
/// external file, no fragile relative path.
///
/// `alignment` is also kept OUT of the public recipe catalog
/// (`sovereign-recipes/registry.toml`) and the bundled-enum
/// (`corpus_engine::recipe_builtin`): it syncs the author's own
/// `~/.claude`, so it must not surface in `sovereign corpus list` or be
/// installable by strangers who happen to run the binary. The trade-off
/// is that the daemon's `fetch_recipe("alignment")` has no catalog entry
/// and no bundled fallback to resolve — it errors `No registry entry for
/// corpus 'alignment'`. We close that gap the way the resolver's tier-1
/// override path is designed for: stage this recipe into the daemon's
/// override dir (`~/.sovereign/recipes/alignment/recipe.toml`) before
/// submitting the install, so `fetch_recipe` resolves it locally without
/// ever needing a public catalog row. The command that owns the
/// alignment workflow owns its recipe — no shared-registry surface area.
const BUNDLED_RECIPE: &str = r#"[corpus]
id = "alignment"
name = "Alignment workspace"
description = """
The user's `~/.claude/` plan files, auto-memory entries, and plan template, \
ingested as a mutable-merge corpus that mesh-replicates between the user's \
own daemons. Pairs with `mutable_merge = "source_doc_id_newest_mtime"` so \
two machines that edit the same memory or plan file converge on the newer \
copy after a mesh tick. The post-merge projector materializes the resulting \
chunks back to disk on the receiving daemon — so a fresh machine reaches \
parity with one `sovereign corpus install alignment`.\
"""
license = "private"
mesh_sharing = true
# Tailscale-IP is the auth boundary; advertising the corpus only exposes
# it to peers already in the user's mesh. Privacy stays structural.
mutable_merge = "source_doc_id_newest_mtime"
size_compressed_gb = 0.001
size_indexed_gb = 0.005

# `local_file` resolves to the directory the extractor walks. The
# extractor's own walk restricts itself to the canonical alignment
# subset (plans/, projects/-*/memory/, _TEMPLATE.md), so pointing at
# `~/.claude` is safe even though it contains other surfaces.
[acquire]
type = "local_file"
path = "~/.claude"

[extract]
type = "alignment_workspace"

# One chunk per markdown file. The extractor populates `mtime` via
# metadata; passthrough preserves it. Mutable-merge keys on
# `source_doc_id` (the relative path), so two machines editing the
# same `plans/today.md` converge on the higher-mtime version.
[chunk]
type = "passthrough"

[index]
fts = true
vector = true

# No enrichment — the alignment corpus is a transport, not an atlas.
[enrichment]
enabled = false
"#;

/// Write the embedded alignment recipe into the daemon's tier-1 recipe
/// override directory so `fetch_recipe(CORPUS_ID)` resolves it before it
/// reaches the (deliberately absent) registry entry. Idempotent: rewrites
/// on every migrate so a recipe.toml edit ships without a manual copy.
///
/// Path shape matches the resolver's subdir layout
/// (`<overrides_dir>/<id>/recipe.toml`, `registry.rs::fetch_recipe` step
/// 1) — the daemon builds `overrides_dir` as `~/.sovereign/recipes`
/// (`CorpusEngine::new(recipes_dir=data_dir.join("recipes"))`).
fn stage_recipe(home: &Path) -> Result<PathBuf, String> {
    let recipe_dir = home.join(".sovereign").join("recipes").join(CORPUS_ID);
    std::fs::create_dir_all(&recipe_dir)
        .map_err(|e| format!("mkdir {}: {e}", recipe_dir.display()))?;
    let recipe_path = recipe_dir.join("recipe.toml");
    std::fs::write(&recipe_path, BUNDLED_RECIPE)
        .map_err(|e| format!("write {}: {e}", recipe_path.display()))?;
    Ok(recipe_path)
}

pub async fn run_alignment(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    match args[0].as_str() {
        "migrate" => cmd_migrate(&args[1..]).await,
        "status" => cmd_status(&args[1..]).await,
        other => {
            eprintln!("Unknown alignment subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn alignment",
    summary: "Manage the mesh-replicated alignment workspace (~/.claude/ + notes.db).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn alignment <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            (
                "migrate [--dry-run]",
                "Back up local state, kick off the alignment corpus ingest, \
                 and show what's in scope. Mesh peers converge automatically \
                 once both have ingested.",
            ),
            (
                "status",
                "Show local alignment scope (files + notes) and ingest state.",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Sync mechanics: this command lands the LOCAL state on the alignment \
             corpus. Cross-machine convergence happens via the daemon's existing \
             mesh hooks (auto_recover, index_transfer); the projector materializes \
             received chunks back to ~/.claude/ + notes.db without operator action.\n\n\
             Reconciliation: newest mtime wins, per file and per notes row. Run \
             this command on BOTH machines (order doesn't matter) to make each \
             advertise its current state to the other.",
        ),
    ],
};

// ─── migrate ────────────────────────────────────────────────────────

async fn cmd_migrate(args: &[String]) -> i32 {
    let mut dry_run = false;
    for a in args {
        match a.as_str() {
            "--dry-run" | "-n" => dry_run = true,
            "--help" | "-h" => {
                println!(
                    "Usage: svrn alignment migrate [--dry-run]\n\n\
                     With --dry-run: walks the alignment scope and prints what \
                     would be exported (files + notes rows), without writing a \
                     backup or touching the daemon. Useful as a sanity check \
                     before the real run.\n\n\
                     Without --dry-run: tar a backup to \
                     ~/.sovereign/backups/alignment-pre-migrate-<ts>.tar, \
                     submit a corpus install request for the `alignment` \
                     recipe, and return. The daemon completes the ingest \
                     and any peer pulls in the background."
                );
                return 0;
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
    }

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot determine home directory; aborting.");
            return 1;
        }
    };

    let scope = match AlignmentScope::scan(&home) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Scan failed: {e}");
            return 1;
        }
    };
    print_scope_summary(&scope, dry_run);

    if dry_run {
        println!();
        println!("Dry run — nothing was backed up, nothing was ingested.");
        return 0;
    }

    let backup = match make_backup(&home, &scope) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Backup failed: {e}");
            eprintln!(
                "Aborting — no ingest will run until the backup step succeeds. \
                 If you really want to proceed unprotected, copy the inputs \
                 yourself and re-run with --dry-run to confirm scope."
            );
            return 2;
        }
    };
    println!("✓ backup at {}", backup.display());

    // Stage the recipe into the daemon's override dir first. `alignment`
    // has no public catalog entry by design (see BUNDLED_RECIPE), so the
    // daemon can only resolve it from this local override — skip this and
    // the install returns `spawned:false` with `No registry entry for
    // corpus 'alignment'` in the daemon log.
    let staged = match stage_recipe(&home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to stage the alignment recipe: {e}");
            eprintln!(
                "Aborting — the daemon cannot resolve the `{CORPUS_ID}` recipe \
                 without it. Backup at {} is intact.",
                backup.display()
            );
            return 2;
        }
    };
    println!("✓ recipe staged at {}", staged.display());

    println!("→ submitting corpus install for `{CORPUS_ID}`");
    let install_code =
        crate::corpus_cmd::run_corpus(&["install".to_string(), CORPUS_ID.to_string()]).await;
    if install_code != 0 {
        eprintln!(
            "corpus install returned exit code {install_code}; backup at {} \
             can be restored with `tar -xf <backup> -C /` if needed.",
            backup.display()
        );
        return install_code;
    }

    println!();
    println!("Ingest submitted to the daemon. Cross-machine convergence happens");
    println!("automatically: peers gossip the new partition, pull each other's");
    println!("state, and the post-merge projector materializes received chunks");
    println!("back to ~/.claude/ + ~/.sovereign/notes.db. Run `svrn alignment");
    println!("status` later to confirm the chunk count matches your peer.");
    0
}

// ─── status ─────────────────────────────────────────────────────────

async fn cmd_status(_args: &[String]) -> i32 {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Cannot determine home directory; aborting.");
            return 1;
        }
    };
    let scope = match AlignmentScope::scan(&home) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Scan failed: {e}");
            return 1;
        }
    };
    print_scope_summary(&scope, true);

    // On-disk corpus (persisted only after the first index flush).
    let mut have_local = false;
    if let Some(indexes_dir) = mesh_indexes_dir() {
        let canonical = indexes_dir.join(CORPUS_ID);
        let meta = canonical.join("_corpus_meta.json");
        if meta.exists() {
            have_local = true;
            println!();
            println!("Local alignment corpus: {}", canonical.display());
            if let Ok(raw) = std::fs::read_to_string(&meta) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(updated_at) = value.get("last_updated").and_then(|v| v.as_u64()) {
                        println!("  last_updated: {}", format_unix_secs(updated_at as i64));
                    }
                    if let Some(policy) = value.get("mutable_merge").and_then(|v| v.as_str()) {
                        println!("  mutable_merge: {policy}");
                    } else {
                        println!(
                            "  ⚠ mutable_merge not stamped — re-ingest with the \
                             current sovereign-cli to enable newest-mtime sync."
                        );
                    }
                }
            }
        }
    }

    // Live in-flight ingest, straight from the daemon — the authoritative
    // "is it running?" signal. The on-disk `_corpus_meta.json` above only
    // appears after the first index flush, so before this check `status`
    // reported "No local alignment corpus yet — run migrate" *while an
    // ingest was actively running*, telling the user to re-launch the very
    // job already in flight (observed 2026-07-24). Query the same progress
    // map the Desktop UI polls so the two surfaces can never disagree.
    match fetch_alignment_progress().await {
        Ok(Some(IngestProgress::Complete {
            total_chunks,
            duration_secs,
        })) => {
            println!();
            println!(
                "✓ Ingest complete — {total_chunks} chunks in {duration_secs}s. \
                 The corpus registers once the projector materializes it."
            );
        }
        Ok(Some(progress)) => {
            println!();
            println!(
                "⏳ Ingest in progress — {}",
                render_ingest_progress(&progress)
            );
            println!("   Re-run `svrn alignment status` to refresh.");
            println!(
                "   Live log: tail -f {} | grep '[alignment]'",
                daemon_log_path()
            );
        }
        Ok(None) => {
            if !have_local {
                println!();
                println!(
                    "No local alignment corpus yet, and no ingest is running on the \
                     daemon. Run `svrn alignment migrate` to ingest the local state."
                );
            }
        }
        Err(reason) => {
            // Daemon unreachable — say so honestly rather than claiming
            // nothing is running (we genuinely can't tell from here).
            if !have_local {
                println!();
                println!(
                    "No local alignment corpus yet. Could not reach the daemon to \
                     check for an in-flight ingest ({reason}). Is `svrn daemon` \
                     running? Try: svrn daemon status"
                );
            }
        }
    }

    0
}

/// GET the daemon's live ingest-progress snapshot (`/internal/corpus/progress`,
/// the same map the Desktop UI polls) and return the entry for the alignment
/// corpus, if any.
///
/// - `Ok(Some(_))` — an ingest for `alignment` is in flight (or just
///   completed and not yet evicted).
/// - `Ok(None)` — daemon reachable but not ingesting alignment.
/// - `Err(_)` — daemon unreachable, so we genuinely don't know.
///
/// Deliberately short-timeout: `status` must stay responsive even when the
/// daemon is busy embedding.
async fn fetch_alignment_progress() -> Result<Option<IngestProgress>, String> {
    #[derive(serde::Deserialize)]
    struct ProgressSnapshot {
        progress: HashMap<String, IngestProgress>,
    }
    // Same internal port the install path posts to (config.node.internal_port,
    // default 9742). Keep the two in lockstep — they talk to the same daemon.
    let url = "http://127.0.0.1:9742/internal/corpus/progress";
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("daemon returned {}", resp.status()));
    }
    let snap: ProgressSnapshot = resp.json().await.map_err(|e| e.to_string())?;
    Ok(snap.progress.get(CORPUS_ID).cloned())
}

/// One-line human rendering of an in-flight ingest phase, for `status`.
fn render_ingest_progress(p: &IngestProgress) -> String {
    match p {
        IngestProgress::Downloading {
            percent,
            bytes_downloaded,
            ..
        } => format!(
            "downloading {percent:.0}% ({:.0} MB)",
            *bytes_downloaded as f64 / 1_048_576.0
        ),
        IngestProgress::Extracting {
            documents_processed,
        } => format!("extracting — {documents_processed} documents scanned"),
        IngestProgress::Chunking { chunks_created } => {
            format!("chunking — {chunks_created} chunks created")
        }
        IngestProgress::Embedding {
            chunks_embedded,
            total,
            docs_processed,
            chunks_per_sec,
            expected_docs,
        } => {
            let mut s = format!(
                "embedding — {docs_processed} docs, {chunks_embedded} chunks @ {chunks_per_sec:.1}/s"
            );
            // Prefer the doc-ratio denominator for filtered ingests (the only
            // honest one), else the chunk total when it's known.
            if let Some(exp) = expected_docs.filter(|e| *e > 0) {
                let pct = (*docs_processed as f64 / exp as f64 * 100.0).min(100.0);
                s.push_str(&format!(" ({pct:.0}% of ~{exp} docs)"));
            } else if *total > 0 {
                let pct = (*chunks_embedded as f64 / *total as f64 * 100.0).min(100.0);
                s.push_str(&format!(" ({pct:.0}% of {total} chunks)"));
            }
            s
        }
        IngestProgress::Indexing {
            chunks_indexed,
            total,
        } => format!("indexing — {chunks_indexed}/{total} chunks"),
        IngestProgress::OptimizingIndex { current_chunks } => {
            format!("optimizing search index ({current_chunks} chunks)")
        }
        IngestProgress::Enriching {
            phase,
            detail,
            fraction,
        } => match fraction {
            Some(f) => format!("enriching [{phase}] {detail} ({:.0}%)", f * 100.0),
            None => format!("enriching [{phase}] {detail}"),
        },
        IngestProgress::Complete {
            total_chunks,
            duration_secs,
        } => format!("complete — {total_chunks} chunks in {duration_secs}s"),
    }
}

/// Path to the live daemon log for the "watch progress" hint. Derived from
/// `sovereign_root()` (the branded runtime home, `~/.svrnmesh`) so it tracks
/// the `svrnmesh` rename — the daemon writes `[alignment] …` progress lines
/// to `daemon.err` there. (The legacy `~/.sovereign/logs/daemon.log` is frozen
/// post-rename, which is what made this ingest's progress invisible on
/// 2026-07-24.)
fn daemon_log_path() -> String {
    sovereign_cli_shared::dirs::sovereign_root()
        .join("logs")
        .join("daemon.err")
        .display()
        .to_string()
}

// ─── shared scaffolding ─────────────────────────────────────────────

struct AlignmentScope {
    plans_dir: PathBuf,
    memory_dirs: Vec<PathBuf>,
    notes_db: Option<PathBuf>,
    file_count: usize,
    note_count: usize,
    by_section: HashMap<&'static str, usize>,
}

impl AlignmentScope {
    fn scan(home: &Path) -> Result<Self, String> {
        let claude_dir = home.join(".claude");
        if !claude_dir.exists() {
            return Err(format!(
                "{} does not exist — nothing to migrate",
                claude_dir.display()
            ));
        }
        let plans_dir = claude_dir.join("plans");

        // Run the actual extractor so the dry-run report matches what
        // the ingest path will produce.
        let extractor = AlignmentWorkspaceExtractor;
        let mut file_count = 0usize;
        let mut note_count = 0usize;
        let mut by_section: HashMap<&'static str, usize> = HashMap::new();
        let iter = extractor
            .extract(&claude_dir)
            .map_err(|e| format!("alignment extractor: {e}"))?;
        for doc in iter {
            let doc = doc.map_err(|e| format!("alignment extractor: {e}"))?;
            if doc.source_id.starts_with("notes://") {
                note_count += 1;
                *by_section.entry("notes").or_insert(0) += 1;
            } else if doc.source_id.starts_with("plans/") {
                file_count += 1;
                *by_section.entry("plans").or_insert(0) += 1;
            } else if doc.source_id.starts_with("projects/") {
                file_count += 1;
                *by_section.entry("memory").or_insert(0) += 1;
            } else {
                file_count += 1;
                *by_section.entry("other").or_insert(0) += 1;
            }
        }

        let mut memory_dirs = Vec::new();
        let projects = claude_dir.join("projects");
        if projects.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&projects) {
                for e in entries.flatten() {
                    let mem = e.path().join("memory");
                    if mem.is_dir() {
                        memory_dirs.push(mem);
                    }
                }
            }
        }

        let notes_db = home.join(".sovereign").join("notes.db");
        let notes_db = if notes_db.exists() {
            Some(notes_db)
        } else {
            None
        };

        Ok(Self {
            plans_dir,
            memory_dirs,
            notes_db,
            file_count,
            note_count,
            by_section,
        })
    }
}

fn print_scope_summary(scope: &AlignmentScope, dry_run: bool) {
    let mode = if dry_run { "dry-run" } else { "scope" };
    println!("Alignment {mode}");
    println!("  plans dir:   {}", scope.plans_dir.display());
    if scope.memory_dirs.is_empty() {
        println!("  memory dirs: (none)");
    } else {
        println!("  memory dirs:");
        for d in &scope.memory_dirs {
            println!("    - {}", d.display());
        }
    }
    match &scope.notes_db {
        Some(p) => println!("  notes.db:    {}", p.display()),
        None => println!("  notes.db:    (not present — markdown only)"),
    }
    println!();
    println!("  files in scope: {}", scope.file_count);
    println!("  notes in scope: {}", scope.note_count);
    if !scope.by_section.is_empty() {
        let mut keys: Vec<&&str> = scope.by_section.keys().collect();
        keys.sort();
        println!();
        for k in keys {
            println!("    {} : {}", k, scope.by_section[k]);
        }
    }
}

fn make_backup(home: &Path, scope: &AlignmentScope) -> Result<PathBuf, String> {
    let backups = home.join(".sovereign").join("backups");
    std::fs::create_dir_all(&backups).map_err(|e| format!("mkdir {}: {e}", backups.display()))?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let out = backups.join(format!("alignment-pre-migrate-{ts}.tar"));

    let mut cmd = std::process::Command::new("tar");
    cmd.arg("-cf").arg(&out);
    // tar's -C means "change to this dir before reading the next path"
    // — using $HOME relative paths makes the tar restorable with
    // `tar -xf <out> -C $HOME` regardless of where the user runs it.
    cmd.arg("-C").arg(home);
    cmd.arg(".claude/plans");
    let projects_subdir = home.join(".claude").join("projects");
    if projects_subdir.is_dir() {
        cmd.arg(".claude/projects");
    }
    if let Some(notes_db) = &scope.notes_db {
        if let Ok(rel) = notes_db.strip_prefix(home) {
            cmd.arg(rel);
        }
    }
    let status = cmd.status().map_err(|e| format!("spawn tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar exited with status {status}"));
    }
    Ok(out)
}

fn mesh_indexes_dir() -> Option<PathBuf> {
    // Corpora live under the runtime root (`sovereign_root()/indexes`,
    // i.e. `~/.svrnmesh/indexes`) — the same root the daemon's corpus
    // engine writes to and `project serve` reads from. The old
    // `mesh_data_dir().join("indexes")` pointed at the XDG *data* dir
    // (`~/.local/share/svrnmesh/indexes`), which only holds mesh identity
    // (mesh.json, node_id) and never any corpus — so `status` reported
    // "No local alignment corpus yet" even after a fully-materialized
    // 1930-chunk ingest sitting in `~/.svrnmesh/indexes/alignment`
    // (observed 2026-07-24).
    Some(sovereign_cli_shared::dirs::sovereign_indexes())
}

fn format_unix_secs(secs: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| secs.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The rendering is what a user reads to answer "is my ingest running?".
    // Pin each phase, and especially the two Embedding denominators, so the
    // percent math can't silently drift.

    #[test]
    fn embedding_prefers_expected_docs_ratio() {
        let p = IngestProgress::Embedding {
            chunks_embedded: 1280,
            total: 0,
            docs_processed: 25,
            chunks_per_sec: 13.7,
            expected_docs: Some(100),
        };
        let s = render_ingest_progress(&p);
        assert!(s.contains("25 docs"), "{s}");
        assert!(s.contains("1280 chunks"), "{s}");
        assert!(s.contains("13.7/s"), "{s}");
        assert!(s.contains("25% of ~100 docs"), "{s}");
    }

    #[test]
    fn embedding_falls_back_to_chunk_total_when_no_expected_docs() {
        let p = IngestProgress::Embedding {
            chunks_embedded: 500,
            total: 1000,
            docs_processed: 500,
            chunks_per_sec: 12.0,
            expected_docs: None,
        };
        let s = render_ingest_progress(&p);
        assert!(s.contains("50% of 1000 chunks"), "{s}");
    }

    #[test]
    fn embedding_percent_is_clamped_to_100() {
        // Filtered ingests scan the whole source, so docs_processed can
        // overshoot the estimate — the bar must not read "150%".
        let p = IngestProgress::Embedding {
            chunks_embedded: 0,
            total: 0,
            docs_processed: 150,
            chunks_per_sec: 0.0,
            expected_docs: Some(100),
        };
        let s = render_ingest_progress(&p);
        assert!(s.contains("100% of ~100 docs"), "{s}");
    }

    #[test]
    fn embedding_omits_percent_when_denominator_unknown() {
        let p = IngestProgress::Embedding {
            chunks_embedded: 10,
            total: 0,
            docs_processed: 5,
            chunks_per_sec: 1.0,
            expected_docs: None,
        };
        let s = render_ingest_progress(&p);
        assert!(!s.contains('%'), "no denominator known, so no percent: {s}");
    }

    #[test]
    fn renders_every_phase_nonempty() {
        let phases = [
            IngestProgress::Downloading {
                percent: 42.0,
                bytes_downloaded: 5 * 1_048_576,
                bytes_total: Some(10 * 1_048_576),
            },
            IngestProgress::Extracting {
                documents_processed: 7,
            },
            IngestProgress::Chunking { chunks_created: 9 },
            IngestProgress::Indexing {
                chunks_indexed: 3,
                total: 8,
            },
            IngestProgress::OptimizingIndex {
                current_chunks: 100,
            },
            IngestProgress::Enriching {
                phase: "entity-extraction".into(),
                detail: "extracting entities".into(),
                fraction: Some(0.5),
            },
            IngestProgress::Complete {
                total_chunks: 1280,
                duration_secs: 120,
            },
        ];
        for p in &phases {
            assert!(!render_ingest_progress(p).is_empty(), "{p:?}");
        }
    }

    #[test]
    fn enriching_renders_fraction_when_present_and_omits_when_absent() {
        let with = IngestProgress::Enriching {
            phase: "clustering".into(),
            detail: "clustering embeddings".into(),
            fraction: Some(0.5),
        };
        assert!(render_ingest_progress(&with).contains("50%"));
        let without = IngestProgress::Enriching {
            phase: "clustering".into(),
            detail: "clustering embeddings".into(),
            fraction: None,
        };
        assert!(!render_ingest_progress(&without).contains('%'));
    }
}
