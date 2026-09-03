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

/// `--prune-days N` as the operator supplied it. `None` = REFUSED.
///
/// There is no default and no zero. Pruning DELETES superseded dataset
/// versions irreversibly, and a retention window below the age of any
/// in-flight reader can break a concurrent query — so the window is a
/// decision the operator has to state, and a value that states nothing
/// (absent, zero, negative, unparseable) is refused rather than filled in.
///
/// A function rather than an inline `match` so the constraint can be
/// exercised without an installed corpus (ARCH §18.1).
fn parse_prune_days(raw: Option<&String>) -> Option<i64> {
    raw.and_then(|v| v.parse::<i64>().ok()).filter(|d| *d >= 1)
}

/// `--keep-versions N` as the operator supplied it. `None` = REFUSED.
/// Same rule, same reason: keeping zero versions is not a retention policy.
fn parse_keep_versions(raw: Option<&String>) -> Option<usize> {
    raw.and_then(|v| v.parse::<usize>().ok()).filter(|n| *n >= 1)
}

pub async fn run_optimize(args: &[String]) -> i32 {
    let mut target: Option<String> = None;
    let mut all = false;
    let mut prune_days: Option<i64> = None;
    let mut keep_versions: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--all" => all = true,
            "--prune-days" => {
                i += 1;
                match parse_prune_days(args.get(i)) {
                    Some(d) => prune_days = Some(d),
                    None => {
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
            "--keep-versions" => {
                i += 1;
                match parse_keep_versions(args.get(i)) {
                    Some(n) => keep_versions = Some(n),
                    None => {
                        eprintln!("error: --keep-versions requires an integer >= 1");
                        return 2;
                    }
                }
            }
            "-h" | "--help" => {
                println!(
                    "svrn corpus optimize <id> [--prune-days N] [--keep-versions N]\n\
                     svrn corpus optimize --all [--prune-days N] [--keep-versions N]\n\n\
                     Compact fragments and fold unindexed fragments into existing indexes.\n\
                     Non-destructive by default: compaction and index optimization only ADD a\n\
                     new dataset version, leaving earlier ones readable.\n\n\
                     --prune-days N      ALSO delete superseded versions older than N days.\n\
                                         DESTRUCTIVE and irreversible.\n\
                     --keep-versions N   ALSO delete superseded versions beyond the newest N,\n\
                                         however recent they are — subject to --prune-days,\n\
                                         which is a floor this can never undercut.\n\n\
                     AN AGE ALONE MAY RECLAIM NOTHING. A corpus fed by a continuous appender\n\
                     can write thousands of versions a day, so every version it retains is\n\
                     newer than any age you can safely pass and --prune-days has no eligible\n\
                     target. Measured on wikipedia 2026-08-31: 5,972 versions, ZERO older than\n\
                     7 days, 153.9GB of superseded fragments. --keep-versions is the bound for\n\
                     that case.\n\n\
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
        eprintln!(
            "usage: svrn corpus optimize <id> [--prune-days N] [--keep-versions N]   (or --all)"
        );
        return 2;
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
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

    // `--keep-versions` alone still needs an age floor to stand on: it can only
    // ever reach FURTHER BACK than the floor, never nearer (`Retention`). Use
    // the flag when given, else the smallest value the flag itself accepts, so
    // an operator asking only for a count bound gets one that can actually bite
    // without being handed a reader-safety decision they did not make.
    let retention = match (prune_days, keep_versions) {
        (None, None) => None,
        (days, keep) => Some(corpus_engine::Retention {
            min_age_days: days.unwrap_or(1),
            keep_versions: keep,
        }),
    };
    if let Some(r) = retention {
        println!(
            "PRUNING ENABLED — superseded versions older than {} day(s) will be DELETED{}.\n",
            r.min_age_days,
            match r.keep_versions {
                Some(k) => format!(", keeping at most the newest {k}"),
                None => String::new(),
            }
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
        match idx.optimize(retention).await {
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
                if let (Some(r), 0) = (retention, stats.old_versions_removed) {
                    // A REQUESTED prune that deleted nothing, reported FIRST
                    // because it outranks every other note here. This command
                    // shipped able to exit 0 having reclaimed zero bytes while
                    // the directory GREW: on 2026-08-31 `--prune-days 7` did
                    // exactly that on wikipedia (+12GB, 0 pruned) because all
                    // 5,972 of its versions were younger than the window, and
                    // nothing in the output said so. A no-op must never read
                    // as a success (ARCH §18.1, §18.3).
                    println!(
                        "   NOTE: pruning was REQUESTED and reclaimed NOTHING. Every one of this\n\
                         \x20        corpus's {} retained versions is newer than the {}-day floor,\n\
                         \x20        so there was no eligible target.{}",
                        after.versions,
                        r.min_age_days,
                        if r.keep_versions.is_none() {
                            "\n            An age cannot bound a continuously-appended corpus — \
                             pass\n            `--keep-versions N` to bound it by count instead."
                        } else {
                            "\n            The count bound was slack too: it can only reach FURTHER \
                             back\n            than the age floor, never nearer. Lower --prune-days."
                        }
                    );
                    if after.bytes > before.bytes {
                        println!(
                            "   NOTE: on-disk size GREW {:.2} GB in the process — compaction wrote new\n\
                             \x20        fragments and nothing reclaimed the superseded ones.",
                            gb(after.bytes.saturating_sub(before.bytes))
                        );
                    }
                } else if stats.skipped_as_clean && stats.old_versions_removed == 0 {
                    println!(
                        "   NOTE: already maintained — no unindexed rows, nothing compacted. The\n\
                         \x20        index pass was SKIPPED on purpose: it is not idempotent and\n\
                         \x20        would add index versions for no gain."
                    );
                } else if stats.fragments_removed == 0 && stats.old_versions_removed == 0 {
                    println!("   NOTE: nothing moved — this dataset was already maintained.");
                } else if retention.is_none() && after.bytes > before.bytes {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// covers: ST-15
    ///
    /// Compaction is non-destructive; pruning is not. It DELETES superseded
    /// dataset versions, irreversibly, and a value below the age of any
    /// in-flight reader breaks a concurrent query. So the flag has no default
    /// and no zero — an operator has to state a retention window, and the one
    /// they state has to be a real window.
    ///
    /// The refusal happens in argument parsing, before an engine is built or
    /// an index is opened, which is what these assertions rely on: none of
    /// these calls needs an installed corpus to exist. A regression that let a
    /// zero through would instead reach `CorpusEngine::installed_indexes` and
    /// return 1 ("no installed corpus matched") on this host — a different
    /// code, from a different place, which is exactly what makes it visible
    /// here rather than only in production.
    #[tokio::test]
    async fn a_prune_with_no_real_retention_window_is_refused_before_any_index_is_opened() {
        for bad in ["0", "-1", "-7", "seven", "1.5", "", "0x1"] {
            let code = run_optimize(&[
                "some-corpus".to_string(),
                "--prune-days".to_string(),
                bad.to_string(),
            ])
            .await;
            assert_eq!(
                code, 2,
                "--prune-days {bad:?} must be refused as a contract violation, not run"
            );
        }

        // The flag with nothing after it at all — the shape a shell typo
        // produces, and the one most likely to be read as "use the default".
        // There is no default.
        assert_eq!(
            run_optimize(&["some-corpus".to_string(), "--prune-days".to_string()]).await,
            2
        );

        // Same rule for the count bound: `--keep-versions 0` would keep no
        // versions at all.
        for bad in ["0", "-1", "all"] {
            assert_eq!(
                run_optimize(&[
                    "some-corpus".to_string(),
                    "--keep-versions".to_string(),
                    bad.to_string(),
                ])
                .await,
                2,
                "--keep-versions {bad:?} must be refused"
            );
        }
    }

    /// covers: ST-15
    ///
    /// The control for the test above (§18.4): the refusals come from the
    /// retention constraint, not from a parser that refuses everything.
    /// Asserted on the deciders directly so it needs no installed corpus and
    /// touches no data directory.
    #[test]
    fn a_stated_retention_window_is_accepted_and_carried_through_verbatim() {
        assert_eq!(parse_prune_days(Some(&"1".to_string())), Some(1));
        assert_eq!(parse_prune_days(Some(&"7".to_string())), Some(7));
        assert_eq!(parse_prune_days(Some(&"365".to_string())), Some(365));
        // 1 is the smallest window the flag accepts — the boundary, stated,
        // because it is also the floor `--keep-versions` alone stands on.
        assert_eq!(parse_prune_days(Some(&"0".to_string())), None);

        assert_eq!(parse_keep_versions(Some(&"1".to_string())), Some(1));
        assert_eq!(parse_keep_versions(Some(&"500".to_string())), Some(500));
        assert_eq!(parse_keep_versions(Some(&"0".to_string())), None);
        assert_eq!(parse_keep_versions(None), None);
    }
}
