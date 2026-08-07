//! `svrn corpus optimize` — Lance dataset maintenance.
//!
//! Compacts fragments, folds unindexed fragments into existing indexes, and
//! (opt-in) prunes superseded versions. See `corpus_engine::index::maintain`
//! for the measurements that motivated this; the short version is that nothing
//! in this workspace performed Lance maintenance before 2026-08-05, and the
//! most-appended corpus had decayed to 2218ms per search against a comparable
//! static corpus's 100ms.
//!
//! MUTATION RATE IS THE DRIVER, so this is a cadence and not a one-shot.
//! `wikipedia` is fed by the `wikipedia-newsworthy` freshness daemon, which is
//! why it carries 3,955 manifest versions while the static `sep` carries 1. A
//! corpus that is appended to continuously re-earns this maintenance
//! continuously.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use corpus_engine::CorpusEngine;

/// On-disk shape of one dataset — the before/after evidence that maintenance
/// did something. Counted from the filesystem rather than reported by Lance so
/// the numbers are directly comparable to what an operator sees with `ls`.
struct DiskShape {
    fragments: usize,
    versions: usize,
    indices: usize,
    bytes: u64,
}

fn count_dir(p: &Path) -> usize {
    std::fs::read_dir(p).map(|d| d.count()).unwrap_or(0)
}

fn dir_bytes(p: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        let Ok(rd) = std::fs::read_dir(p) else {
            return 0;
        };
        rd.flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path()),
                Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => 0,
            })
            .sum()
    }
    walk(p)
}

/// `InstalledIndex::path` is the CORPUS directory, not the Lance dataset —
/// `open_index` resolves `chunks.lance` inside it. Counting the corpus dir
/// yields zeros for every structural field, which then reads as "nothing
/// moved". Found the hard way on the first real run against wikipedia
/// (2026-08-05): it reported `0 fragments / 0 versions` and printed the
/// no-op note while compaction had in fact merged 4,620 fragments into 3.
fn dataset_dir(corpus_dir: &Path) -> PathBuf {
    let nested = corpus_dir.join("chunks.lance");
    if nested.is_dir() {
        nested
    } else {
        corpus_dir.to_path_buf()
    }
}

fn shape_of(corpus_dir: &Path) -> DiskShape {
    let dataset = dataset_dir(corpus_dir);
    DiskShape {
        fragments: count_dir(&dataset.join("data")),
        versions: count_dir(&dataset.join("_versions")),
        indices: count_dir(&dataset.join("_indices")),
        bytes: dir_bytes(&dataset),
    }
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

pub async fn run_optimize(args: &[String]) -> i32 {
    let mut target: Option<String> = None;
    let mut all = false;
    let mut prune_days: Option<i64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => all = true,
            "--prune-days" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse::<i64>().ok()) {
                    Some(d) if d >= 1 => prune_days = Some(d),
                    _ => {
                        eprintln!("error: --prune-days requires an integer >= 1");
                        eprintln!(
                            "  pruning DELETES superseded dataset versions. A value below the age\n\
                             of any in-flight reader can break a concurrent query, which is why\n\
                             there is no default and no zero."
                        );
                        return 2;
                    }
                }
            }
            "-h" | "--help" => {
                println!(
                    "svrn corpus optimize <id> [--prune-days N]\n\
                     svrn corpus optimize --all [--prune-days N]\n\n\
                     Compact fragments and fold unindexed fragments into existing indexes.\n\
                     Non-destructive by default: compaction and index optimization only ADD a\n\
                     new dataset version, leaving earlier ones readable.\n\n\
                     --prune-days N   ALSO delete superseded versions older than N days.\n\
                                      DESTRUCTIVE and irreversible. Reclaims the storage that\n\
                                      continuous appends leave behind.\n\n\
                     Corpora fed by a continuous appender (e.g. wikipedia via the\n\
                     wikipedia-newsworthy freshness daemon) re-earn this maintenance over time;\n\
                     static corpora need it once."
                );
                return 0;
            }
            other if !other.starts_with('-') && target.is_none() => {
                target = Some(other.to_string())
            }
            other => {
                eprintln!("error: unrecognized argument {other:?}");
                return 2;
            }
        }
        i += 1;
    }
    if target.is_none() && !all {
        eprintln!("usage: svrn corpus optimize <id> [--prune-days N]   (or --all)");
        return 2;
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".sovereign")
        })
        .join("indexes");

    let engine = CorpusEngine::new(
        std::env::temp_dir(),
        indexes_dir.clone(),
        Arc::new(|_: &str| {
            Box::pin(async move { Ok::<Vec<f32>, corpus_engine::Error>(Vec::new()) })
        }),
    );

    let indexes = match engine.installed_indexes().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: list indexes: {e}");
            return 1;
        }
    };

    let selected: Vec<_> = indexes
        .into_iter()
        .filter(|i| target.as_ref().is_none_or(|t| &i.corpus_id == t))
        .collect();
    if selected.is_empty() {
        eprintln!("error: no installed corpus matched");
        return 1;
    }

    if prune_days.is_some() {
        println!(
            "PRUNING ENABLED — superseded versions older than {} day(s) will be DELETED.\n",
            prune_days.unwrap()
        );
    }

    let mut failed = 0;
    for info in selected {
        let dataset = info.path.clone();
        let before = shape_of(&dataset);
        println!("── {} ", info.corpus_id);
        println!(
            "   before: {:>6} fragments  {:>6} versions  {:>3} indices  {:>7.2} GB",
            before.fragments,
            before.versions,
            before.indices,
            gb(before.bytes)
        );

        let idx = match engine.open_index(&dataset).await {
            Ok(i) => i,
            Err(e) => {
                eprintln!("   error: open_index: {e}");
                failed += 1;
                continue;
            }
        };

        let t = std::time::Instant::now();
        match idx.optimize(prune_days).await {
            Ok(stats) => {
                let after = shape_of(&dataset);
                println!(
                    "   after : {:>6} fragments  {:>6} versions  {:>3} indices  {:>7.2} GB   ({:.1}s)",
                    after.fragments,
                    after.versions,
                    after.indices,
                    gb(after.bytes),
                    t.elapsed().as_secs_f64()
                );
                println!(
                    "   compaction: -{} fragments / +{} written · unindexed_rows_before={} · indexes {} · pruned {} version file(s), {:.2} GB",
                    stats.fragments_removed,
                    stats.fragments_added,
                    stats.unindexed_rows_before,
                    if stats.skipped_as_clean {
                        "SKIPPED (already folded in)"
                    } else if stats.indexes_optimized {
                        "optimized"
                    } else {
                        "not optimized"
                    },
                    stats.old_versions_removed,
                    gb(stats.bytes_removed),
                );
                // Report the absence of change rather than letting a no-op read
                // as a success (ARCH §18.3). Keyed on Lance's own stats, NOT on
                // the filesystem counts: compaction is non-destructive, so the
                // superseded fragments stay on disk until a prune and the
                // directory counts barely move even on a large compaction.
                if stats.skipped_as_clean && stats.old_versions_removed == 0 {
                    println!(
                        "   NOTE: already maintained — no unindexed rows, nothing compacted. The\n\
                         \x20        index pass was SKIPPED on purpose: it is not idempotent and\n\
                         \x20        would add index versions for no gain."
                    );
                } else if stats.fragments_removed == 0 && stats.old_versions_removed == 0 {
                    println!("   NOTE: nothing moved — this dataset was already maintained.");
                } else if prune_days.is_none() && after.bytes > before.bytes {
                    // Say this out loud: an operator watching disk USAGE GROW
                    // after running a "cleanup" command deserves the reason.
                    println!(
                        "   NOTE: on-disk size GREW {:.2} GB. Expected — compaction wrote new\n\
                         \x20        fragments while the superseded ones stay readable under the old\n\
                         \x20        manifest versions. `--prune-days N` is what reclaims them.",
                        gb(after.bytes.saturating_sub(before.bytes))
                    );
                }
            }
            Err(e) => {
                eprintln!("   error: optimize: {e}");
                failed += 1;
            }
        }
        println!();
    }

    if failed > 0 {
        eprintln!("{failed} corpus/corpora failed maintenance");
        return 1;
    }
    0
}
