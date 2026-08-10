// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus snapshot` — publish and inspect prebuilt-index
//! tarballs (.tar.zst) for cold-start onboarding.
//!
//! See `corpus-engine/src/snapshot.rs` for the manifest format and
//! the `publish_snapshot` engine entry point. This module is a thin
//! arg-parsing layer that gathers paths, counts chunks, drives the
//! engine, and optionally shells out to `huggingface-cli` for upload.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use corpus_engine::snapshot::{
    prebuilt_toml_snippet, publish_snapshot, read_manifest_from_archive, restore_snapshot_archive,
    PublishOptions, SnapshotManifest,
};
use corpus_engine::CorpusIndex;

use sovereign_cli_shared::help::{Help, HelpSection};

/// Branded per-user data root (rebrand-aware path SSOT — prefers a
/// populated `~/.svrnmesh`, honors `SOVEREIGN_DATA_DIR` via callers of
/// `rebrand::data_dir`; derivation lives in sovereign-cli-shared).
fn sovereign_root() -> PathBuf {
    sovereign_cli_shared::dirs::sovereign_root()
}

const PRODUCER_VERSION: &str = concat!("sovereign-cli/", env!("CARGO_PKG_VERSION"));

const HELP_SNAPSHOT: Help = Help {
    command: "svrn corpus snapshot",
    summary: "Publish, inspect, and restore prebuilt-index tarballs for cold-start onboarding.",
    sections: &[
        HelpSection::Usage("svrn corpus snapshot <subcommand> [args]"),
        HelpSection::Subcommands(&[
            ("publish <id>", "Build a .tar.zst snapshot of an installed corpus and (optionally) upload to HuggingFace"),
            ("inspect <archive>", "Print the manifest from an existing snapshot archive"),
            ("restore <ref>", "Download (HF) and extract a snapshot. Used to validate the cold-start path before promoting it via the recipe's [prebuilt] block."),
        ]),
        HelpSection::Notes(
            "`publish` writes to ~/.svrnmesh/snapshots/<filename>.tar.zst by default.\n\
             Including the atlas adds ~2 GB for Wikipedia but is what restorers will expect.",
        ),
    ],
};

const HELP_SNAPSHOT_RESTORE: Help = Help {
    command: "svrn corpus snapshot restore",
    summary: "Download a snapshot from HuggingFace (or use a local file) and extract it under `~/.svrnmesh/`.",
    sections: &[
        HelpSection::Usage(
            "svrn corpus snapshot restore <hf_repo>/<filename> [flags]\n\
             svrn corpus snapshot restore --archive <path> [flags]",
        ),
        HelpSection::Flags(&[
            ("--archive <path>", "Use a local .tar.zst instead of fetching from HF"),
            ("--as <corpus_id>", "Rename the corpus on restore (lands under indexes/<this id>/). Default: archive's manifest.corpus_id."),
            ("--into <dir>", "Sovereign data root (default ~/.svrnmesh/)"),
            ("--expected-sha256 <hex>", "Gate restore on this archive sha256 (recommended for the production path)"),
            ("--embedding-model <name>", "Compatibility check against this model name (default: qwen-embedding-0.6b)"),
            ("--embedding-dim <n>", "Compatibility check against this vector dimensionality (default: 1024)"),
        ]),
        HelpSection::Notes(
            "After a successful restore, validate with `svrn corpus diag <corpus_id>`.\n\
             For empirical testing of the cold-start path, use `--as <something>-prebuilt-test`\n\
             so the existing install isn't touched.",
        ),
    ],
};

const HELP_SNAPSHOT_PUBLISH: Help = Help {
    command: "svrn corpus snapshot publish",
    summary: "Package an installed corpus into a .tar.zst snapshot for distribution. Build is resumable; upload retries with backoff.",
    sections: &[
        HelpSection::Usage("svrn corpus snapshot publish <corpus_id> [flags]"),
        HelpSection::Flags(&[
            ("--no-atlas", "Skip the enrichment/<id>/ subtree (smaller archive, restorers must re-enrich)"),
            ("--output <path>", "Where to write the archive (default ~/.svrnmesh/snapshots/<filename>)"),
            ("--snapshot-id <id>", "Override the auto-generated snapshot id (default: <corpus>-<model>-<date>)"),
            ("--notes <text>", "Free-form notes recorded in the manifest"),
            ("--residual-gap-pct <f>", "Known incompleteness percent (e.g. 2.81 for wikipedia)"),
            ("--zstd-level <int>", "Zstd compression level (default 19 — high ratio, slower; use 3 for fast)"),
            ("--upload <repo>", "Upload to HuggingFace via `hf upload` (e.g. svrnmesh/wikipedia-index)"),
            ("--upload-max-attempts <n>", "Retry the HF upload up to N times with exponential backoff (default 5)"),
            ("--rebuild", "Force a fresh tar even if a complete archive is already at the output path"),
            ("--upload-only", "Skip the build entirely; require an existing archive at the output path"),
            ("--dry-run", "Print the upload command instead of running it"),
            ("--include-siblings <prefix>", "Bundle every installed corpus whose id starts with <prefix> (e.g. 'sep-') alongside the primary index. Used by per-article-corpus pipelines like SEP so one tarball carries the parent + all 1770 per-article atlases. Repeatable. Trailing '*' tolerated."),
            ("--allow-unjoined-sections", "Publish even when a bundled corpus declares sections but has no chunk→section join. Publishing REFUSES by default: chapters.json travels in the bundle but the source document does not, so a downloader cannot compute the join and the corpus can never name a section in a citation. Fix with `svrn enrich backfill-sections --all` instead of reaching for this."),
        ]),
        HelpSection::Notes(
            "Resumable: an interrupted build leaves `<output>.part`; a complete build leaves\n\
             `<output>`. Re-running the same command skips a complete build and retries the\n\
             upload (HF's multipart resume handles already-sent chunks). For an upload-only\n\
             retry after a frozen `hf upload`, just re-run the same command — the build is\n\
             skipped automatically.\n\n\
             Pre-flight: run `svrn corpus diag <id>` first to confirm completeness;\n\
             the resulting count goes into the manifest. After publish, paste the printed\n\
             [prebuilt] block into sovereign-recipes/<id>/recipe.toml.",
        ),
    ],
};

const HELP_SNAPSHOT_INSPECT: Help = Help {
    command: "svrn corpus snapshot inspect",
    summary: "Print the manifest stored at the root of a snapshot archive.",
    sections: &[HelpSection::Usage(
        "svrn corpus snapshot inspect <archive.tar.zst>",
    )],
};

pub async fn run_snapshot(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP_SNAPSHOT);
        return if args.is_empty() { 1 } else { 0 };
    }
    match args[0].as_str() {
        "publish" => cmd_publish(&args[1..]).await,
        "inspect" => cmd_inspect(&args[1..]),
        "restore" => cmd_restore(&args[1..]).await,
        other => {
            eprintln!("Unknown snapshot subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP_SNAPSHOT);
            1
        }
    }
}

#[derive(Default)]
struct PublishArgs {
    corpus_id: Option<String>,
    include_atlas: bool,
    output: Option<PathBuf>,
    snapshot_id: Option<String>,
    notes: Option<String>,
    residual_gap_pct: Option<f32>,
    zstd_level: i32,
    upload_repo: Option<String>,
    dry_run: bool,
    /// Force rebuild even when a complete archive is already on disk.
    /// Without this, an existing archive at the resolved output path
    /// is reused (sha re-verified) and the upload phase retries —
    /// this is the resumable-after-frozen-HF-upload happy path.
    rebuild: bool,
    /// Skip the build entirely; only run the upload. Errors if the
    /// archive isn't already at the resolved output path.
    upload_only: bool,
    /// How many times to retry `hf upload` on failure, with
    /// exponential backoff between attempts.
    upload_max_attempts: u32,
    /// Sibling-corpus prefix to bundle alongside the primary index.
    /// E.g. `--include-siblings sep-` bundles every corpus whose id
    /// starts with `sep-` (the 1770 per-article SEP atlases). Trailing
    /// `*` is stripped. Each match tars under `indexes/<sibling_id>/`
    /// and the id list is recorded in the manifest's
    /// `bundled_corpora`.
    include_siblings: Vec<String>,
    /// Publish anyway when a bundled corpus declares sections but carries
    /// no chunk→section join. Off by default, and deliberately awkward to
    /// type: see [`audit_section_joins`] for why an unjoined corpus is a
    /// defect that only the PUBLISHER can fix.
    allow_unjoined_sections: bool,
}

fn parse_publish_args(args: &[String]) -> std::result::Result<PublishArgs, String> {
    let mut out = PublishArgs {
        include_atlas: true,
        zstd_level: 19,
        upload_max_attempts: 5,
        ..Default::default()
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--no-atlas" => out.include_atlas = false,
            "--output" => {
                let v = iter.next().ok_or("--output requires a path")?;
                out.output = Some(PathBuf::from(v));
            }
            "--snapshot-id" => {
                let v = iter.next().ok_or("--snapshot-id requires a value")?;
                out.snapshot_id = Some(v.clone());
            }
            "--notes" => {
                let v = iter.next().ok_or("--notes requires a value")?;
                out.notes = Some(v.clone());
            }
            "--residual-gap-pct" => {
                let v = iter.next().ok_or("--residual-gap-pct requires a float")?;
                let parsed: f32 = v
                    .parse()
                    .map_err(|_| format!("--residual-gap-pct: '{v}' is not a number"))?;
                out.residual_gap_pct = Some(parsed);
            }
            "--zstd-level" => {
                let v = iter.next().ok_or("--zstd-level requires an integer")?;
                out.zstd_level = v
                    .parse()
                    .map_err(|_| format!("--zstd-level: '{v}' is not an integer"))?;
            }
            "--upload" => {
                let v = iter.next().ok_or("--upload requires a HuggingFace repo")?;
                out.upload_repo = Some(v.clone());
            }
            "--dry-run" => out.dry_run = true,
            "--allow-unjoined-sections" => out.allow_unjoined_sections = true,
            "--rebuild" => out.rebuild = true,
            "--upload-only" => out.upload_only = true,
            "--include-siblings" => {
                let v = iter
                    .next()
                    .ok_or("--include-siblings requires a corpus-id prefix (e.g. 'sep-')")?;
                // Tolerate `sep-*` and `sep-` interchangeably; both
                // mean the same thing (we don't support full glob).
                let prefix = v.trim_end_matches('*').to_string();
                if prefix.is_empty() {
                    return Err(
                        "--include-siblings requires a non-empty prefix (e.g. 'sep-')".into(),
                    );
                }
                out.include_siblings.push(prefix);
            }
            "--upload-max-attempts" => {
                let v = iter
                    .next()
                    .ok_or("--upload-max-attempts requires an integer")?;
                out.upload_max_attempts = v
                    .parse()
                    .map_err(|_| format!("--upload-max-attempts: '{v}' is not an integer"))?;
                if out.upload_max_attempts == 0 {
                    return Err("--upload-max-attempts must be >= 1".into());
                }
            }
            "--help" | "-h" => return Err("__help__".into()),
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if out.corpus_id.is_some() {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                out.corpus_id = Some(other.to_string());
            }
        }
    }
    if out.rebuild && out.upload_only {
        return Err("--rebuild and --upload-only are mutually exclusive".into());
    }
    Ok(out)
}

async fn cmd_publish(args: &[String]) -> i32 {
    let parsed = match parse_publish_args(args) {
        Ok(p) => p,
        Err(msg) if msg == "__help__" => {
            sovereign_cli_shared::help::print(&HELP_SNAPSHOT_PUBLISH);
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };

    let Some(corpus_id) = parsed.corpus_id else {
        eprintln!("usage: svrn corpus snapshot publish <corpus_id> [flags]");
        return 2;
    };

    let index_dir = sovereign_root().join("indexes").join(&corpus_id);
    if !index_dir.exists() {
        eprintln!("Index directory not found: {}", index_dir.display());
        eprintln!(
            "Install the corpus first with `svrn corpus install {corpus_id}` or pull a partition."
        );
        return 1;
    }
    let enrichment_root = sovereign_root().join("enrichment").join(&corpus_id);
    let atlas_in_index = index_dir.join("atlas").is_dir();
    let enrichment_dir = if parsed.include_atlas && enrichment_root.exists() {
        Some(enrichment_root.clone())
    } else {
        None
    };
    match (
        atlas_in_index,
        enrichment_dir.is_some(),
        parsed.include_atlas,
    ) {
        (true, true, true) => {
            println!(
                "Atlas: capturing from both {}/atlas/ and {}",
                index_dir.display(),
                enrichment_root.display()
            );
        }
        (true, false, true) => {
            println!(
                "Atlas: capturing from {}/atlas/ (in-index location; no separate enrichment dir at {})",
                index_dir.display(),
                enrichment_root.display()
            );
        }
        (false, true, true) => {
            println!("Atlas: capturing from {}", enrichment_root.display());
        }
        (false, false, true) => {
            println!(
                "Atlas: not found at {}/atlas/ or {} — publishing without atlas data",
                index_dir.display(),
                enrichment_root.display()
            );
        }
        (_, _, false) => {
            println!("Atlas: skipped (--no-atlas)");
        }
    }

    println!("Counting chunks in {} ...", index_dir.display());
    let chunk_count = match CorpusIndex::open(&index_dir).await {
        Ok(idx) => match idx.chunk_count().await {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Failed to count chunks: {e}");
                return 1;
            }
        },
        Err(e) => {
            eprintln!("Failed to open index at {}: {e}", index_dir.display());
            return 1;
        }
    };
    println!("  chunks: {chunk_count}");

    let snapshot_id = parsed
        .snapshot_id
        .clone()
        .unwrap_or_else(|| default_snapshot_id(&corpus_id, &index_dir));
    let output_path = parsed.output.clone().unwrap_or_else(|| {
        sovereign_root()
            .join("snapshots")
            .join(format!("{snapshot_id}.tar.zst"))
    });

    // Resumable-publish: an existing archive at output_path is reused
    // unless --rebuild was passed. A sibling `.part` is the marker for
    // an interrupted build — the engine's write_snapshot_archive
    // tar-renames atomically, so if `.part` exists alongside (or
    // without) the final file, the prior build crashed and we must
    // rebuild.
    let part_marker = part_path_for(&output_path);
    let archive_complete = output_path.is_file() && !part_marker.exists();
    let skip_build = parsed.upload_only || (archive_complete && !parsed.rebuild);

    let outcome = if skip_build {
        if !output_path.is_file() {
            eprintln!(
                "--upload-only requires an existing archive at {} — not found",
                output_path.display()
            );
            return 1;
        }
        println!(
            "Reusing existing archive at {} (skipping build; pass --rebuild to force)",
            output_path.display()
        );
        match reconstruct_outcome(&output_path) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("Cannot read existing archive: {e}");
                return 1;
            }
        }
    } else {
        if part_marker.exists() {
            println!(
                "Discarding stale partial archive at {} (prior build interrupted)",
                part_marker.display()
            );
            if let Err(e) = std::fs::remove_file(&part_marker) {
                eprintln!("Warning: could not remove {}: {e}", part_marker.display());
            }
        }
        println!(
            "Building archive at {} (zstd level {}) ...",
            output_path.display(),
            parsed.zstd_level
        );
        println!("  this can take several minutes for multi-GB indexes");

        // Resolve sibling prefixes (`--include-siblings sep-`) against
        // installed indexes. The primary corpus is excluded so a
        // self-matching prefix (e.g. publishing 'sep' with prefix
        // 'sep') doesn't double-bundle it. Sibling list is stable-
        // sorted by id for deterministic manifest output.
        let mut sibling_index_dirs: Vec<(String, PathBuf)> = Vec::new();
        if !parsed.include_siblings.is_empty() {
            let indexes_root = sovereign_root().join("indexes");
            let entries = match std::fs::read_dir(&indexes_root) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("cannot read {}: {e}", indexes_root.display());
                    return 1;
                }
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(id) = name.to_str() else { continue };
                if id == corpus_id {
                    continue;
                }
                if !parsed
                    .include_siblings
                    .iter()
                    .any(|prefix| id.starts_with(prefix))
                {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    sibling_index_dirs.push((id.to_string(), path));
                }
            }
            sibling_index_dirs.sort_by(|a, b| a.0.cmp(&b.0));
            println!(
                "Bundling {} sibling corpus/corpora matching {:?}",
                sibling_index_dirs.len(),
                parsed.include_siblings
            );
        }

        // PUBLISH GATE — the last point at which an unjoined corpus is still
        // OUR problem. Past here it is a download on someone else's machine,
        // where the source document does not exist and the join cannot be
        // recomputed at all.
        let audit = audit_section_joins(&corpus_id, &index_dir, &sibling_index_dirs);
        if !audit.unjoined.is_empty() {
            audit.report(parsed.allow_unjoined_sections);
            if !parsed.allow_unjoined_sections {
                return 1;
            }
        } else if audit.checked > 0 {
            println!(
                "Section joins: {} of {} bundled corpus/corpora carry one.",
                audit.joined, audit.checked
            );
        }

        let opts = PublishOptions {
            index_dir,
            enrichment_dir,
            output_path: output_path.clone(),
            snapshot_id: snapshot_id.clone(),
            chunk_count,
            residual_gap_pct: parsed.residual_gap_pct,
            notes: parsed.notes.clone(),
            source_recipe_sha256: None,
            producer_version: PRODUCER_VERSION.to_string(),
            zstd_level: parsed.zstd_level,
            sibling_index_dirs,
        };

        match publish_snapshot(opts).await {
            Ok(o) => o,
            Err(e) => {
                eprintln!("Publish failed: {e}");
                // Leave .part on disk so the operator can inspect; the
                // next invocation will discard it before retrying.
                return 1;
            }
        }
    };

    println!();
    println!("  archive: {}", outcome.archive_path.display());
    println!(
        "  size:    {:.2} GB ({} bytes)",
        outcome.archive_size_bytes as f64 / 1.073e9_f64,
        outcome.archive_size_bytes
    );
    println!("  sha256:  {}", outcome.archive_sha256);
    println!(
        "  atlas:   {}",
        if outcome.manifest.atlas_included {
            "included"
        } else {
            "not included"
        }
    );
    if !outcome.manifest.bundled_corpora.is_empty() {
        println!(
            "  siblings: {} bundled ({} … {})",
            outcome.manifest.bundled_corpora.len(),
            outcome
                .manifest
                .bundled_corpora
                .first()
                .map(|s| s.as_str())
                .unwrap_or(""),
            outcome
                .manifest
                .bundled_corpora
                .last()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );
    }

    if let Some(repo) = parsed.upload_repo.as_deref() {
        let filename = outcome
            .archive_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("snapshot.tar.zst");
        let target = format!("{repo}/{filename}");
        let cmd_str = format!(
            "hf upload --repo-type=dataset {} {} {}",
            repo,
            outcome.archive_path.display(),
            filename,
        );
        if parsed.dry_run {
            println!();
            println!("Dry-run: would upload to {target} with:");
            println!("  {cmd_str}");
        } else {
            println!();
            println!(
                "Uploading to {target} (max {} attempts) ...",
                parsed.upload_max_attempts
            );
            match run_hf_upload_with_retry(repo, &outcome.archive_path, parsed.upload_max_attempts)
                .await
            {
                Ok(()) => println!("Upload complete."),
                Err(msg) => {
                    eprintln!(
                        "Upload failed after {} attempts: {msg}",
                        parsed.upload_max_attempts
                    );
                    eprintln!(
                        "The archive is still on disk; re-run the same command to retry just the upload \
                         (the build phase will be skipped). Or run manually:"
                    );
                    eprintln!("  {cmd_str}");
                    return 1;
                }
            }
        }
    }

    println!();
    println!(
        "{}",
        prebuilt_toml_snippet(
            &outcome,
            parsed.upload_repo.as_deref().unwrap_or("svrnmesh/<repo>")
        )
    );

    0
}

/// Default snapshot id derived from corpus_id + embedding model
/// (read from `_corpus_meta.json`) + today's UTC date.
fn default_snapshot_id(corpus_id: &str, index_dir: &Path) -> String {
    let model = std::fs::read_to_string(index_dir.join("_corpus_meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| {
            v.get("embedding_model")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown-embed".to_string());
    let date = Utc::now().format("%Y-%m-%d");
    format!("{corpus_id}-{model}-{date}")
}

fn run_hf_upload(repo: &str, archive: &Path) -> std::result::Result<(), String> {
    let filename = archive
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "archive path has no filename".to_string())?;
    // The 2026 huggingface_hub release replaced `huggingface-cli` with
    // the `hf` binary. Prefer the new name; fall back to the old one
    // for older installs that still have it on PATH.
    let candidates = ["hf", "huggingface-cli"];
    let mut last_err = String::new();
    for bin in candidates {
        let result = Command::new(bin)
            .arg("upload")
            .arg("--repo-type=dataset")
            .arg(repo)
            .arg(archive.as_os_str())
            .arg(filename)
            .status();
        match result {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(format!("{bin} upload exited with {status}"));
            }
            Err(e) => {
                last_err = format!("spawn {bin}: {e}");
            }
        }
    }
    Err(format!(
        "neither `hf` nor `huggingface-cli` is on PATH ({last_err}); \
         install via `pip3 install --user huggingface_hub[cli]`"
    ))
}

/// Retry `run_hf_upload` up to `max_attempts` with exponential backoff
/// (15s, 30s, 60s, 120s, 240s capped at 5min). `hf upload` does its
/// own multipart-resume internally — already-uploaded chunks aren't
/// re-sent — so retrying is cheap when an upload merely froze.
async fn run_hf_upload_with_retry(
    repo: &str,
    archive: &Path,
    max_attempts: u32,
) -> std::result::Result<(), String> {
    let mut last_err = String::new();
    for attempt in 1..=max_attempts {
        if attempt > 1 {
            let backoff_secs = (15u64 * (1 << (attempt - 2))).min(300);
            eprintln!(
                "Attempt {attempt}/{max_attempts}: waiting {backoff_secs}s before retry \
                 (previous error: {last_err}) …"
            );
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        }
        eprintln!("Attempt {attempt}/{max_attempts}: hf upload …");
        match run_hf_upload(repo, archive) {
            Ok(()) => return Ok(()),
            Err(msg) => {
                last_err = msg;
            }
        }
    }
    Err(last_err)
}

/// Compute the sibling `.part` path for an output archive. Mirrors
/// the same convention `corpus-engine::snapshot::write_snapshot_archive`
/// writes to.
/// What the publish gate found across the primary corpus and every sibling
/// about to be bundled.
struct JoinAudit {
    /// Corpora that declare sections (i.e. could have a join at all).
    checked: usize,
    joined: usize,
    /// Corpus id + how many sections it declares, for each one that declares
    /// sections and joins NONE of them.
    unjoined: Vec<(String, usize)>,
}

impl JoinAudit {
    /// The operator-facing verdict. Named separately from the check so the
    /// refusal and the `--allow-unjoined-sections` override print the SAME
    /// facts — an override should not be a quieter code path (§18.3).
    fn report(&self, overridden: bool) {
        let verb = if overridden { "SHIPPING ANYWAY" } else { "REFUSING TO PUBLISH" };
        eprintln!();
        eprintln!(
            "{verb}: {} of {} bundled corpus/corpora declare sections but carry NO \
             chunk→section join.",
            self.unjoined.len(),
            self.checked
        );
        for (id, sections) in self.unjoined.iter().take(10) {
            eprintln!("  {id}  ({sections} sections, 0 joined)");
        }
        if self.unjoined.len() > 10 {
            eprintln!("  … and {} more", self.unjoined.len() - 10);
        }
        eprintln!();
        eprintln!(
            "  A downloader receives chapters.json and NO source document, so they cannot"
        );
        eprintln!(
            "  compute this join themselves. Published unjoined, these corpora can never"
        );
        eprintln!("  name a section in a citation, and the defect is unrepairable at their end.");
        eprintln!();
        eprintln!("  Fix here, then re-publish:");
        eprintln!("    svrn enrich backfill-sections --all        # or one corpus id");
        eprintln!();
        if overridden {
            eprintln!(
                "  --allow-unjoined-sections was passed, so the bundle is being built with \
                 the defect above."
            );
        } else {
            eprintln!(
                "  Override with --allow-unjoined-sections if a source document genuinely \
                 is not available."
            );
        }
    }
}

/// Check the primary index and every sibling for a populated chunk→section
/// join.
///
/// Publish is the last moment this is fixable. `chapters.json` travels in the
/// bundle but the SOURCE DOCUMENT does not, and the join is computed by
/// locating chunk text inside that source — so a downloader has no way to
/// recompute it. Shipping unjoined converts a repairable local gap into a
/// permanent property of everyone else's copy.
///
/// Only `JoinStatus::JoinMissing` fails. A corpus with no declared sections
/// has nothing to join and is not a defect; a PARTIAL join is legitimate
/// (a section may genuinely contain no chunk).
fn audit_section_joins(
    corpus_id: &str,
    index_dir: &Path,
    siblings: &[(String, PathBuf)],
) -> JoinAudit {
    use corpus_engine::enrichment::governance_view::{chunk_to_section_map_status, JoinStatus};
    let mut audit = JoinAudit { checked: 0, joined: 0, unjoined: Vec::new() };
    let all = std::iter::once((corpus_id.to_string(), index_dir.to_path_buf()))
        .chain(siblings.iter().cloned());
    for (id, dir) in all {
        let status = chunk_to_section_map_status(&dir);
        match status.status {
            JoinStatus::NoSectionStructure => {}
            JoinStatus::Present => {
                audit.checked += 1;
                audit.joined += 1;
            }
            JoinStatus::JoinMissing => {
                audit.checked += 1;
                audit.unjoined.push((id, status.sections_total));
            }
        }
    }
    audit
}

fn part_path_for(output: &Path) -> PathBuf {
    output.with_extension(
        output
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("{e}.part"))
            .unwrap_or_else(|| "part".into()),
    )
}

/// Reconstruct a `PublishOutcome`-shaped view from an archive file
/// already on disk. Used in the `--upload-only` and skip-build resume
/// paths so the rest of `cmd_publish` (the upload + snippet phase)
/// doesn't need to know whether the archive was just produced or
/// reused from a prior run. Reads the in-archive manifest, recomputes
/// sha256, returns sizes.
fn reconstruct_outcome(
    archive: &Path,
) -> std::result::Result<corpus_engine::snapshot::PublishOutcome, String> {
    use corpus_engine::snapshot::{read_manifest_from_archive, PublishOutcome};

    let manifest = read_manifest_from_archive(archive)
        .map_err(|e| format!("read manifest from {}: {e}", archive.display()))?;
    let (sha, size) = hash_file_sha256(archive)?;
    Ok(PublishOutcome {
        manifest,
        archive_path: archive.to_path_buf(),
        archive_sha256: sha,
        archive_size_bytes: size,
    })
}

/// Compute SHA-256 of a file in 1-MiB blocks. Returns `(hex_digest, size)`.
fn hash_file_sha256(path: &Path) -> std::result::Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut total: u64 = 0;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok((hex, total))
}

fn cmd_inspect(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        sovereign_cli_shared::help::print(&HELP_SNAPSHOT_INSPECT);
        return if args.is_empty() { 2 } else { 0 };
    }
    let archive_path = PathBuf::from(&args[0]);
    let manifest = match read_manifest_from_archive(&archive_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!(
                "Failed to read manifest from {}: {e}",
                archive_path.display()
            );
            return 1;
        }
    };
    print_manifest(&manifest);
    0
}

fn print_manifest(m: &SnapshotManifest) {
    println!("Snapshot manifest");
    println!("  schema_version:         {}", m.schema_version);
    println!("  corpus_id:              {}", m.corpus_id);
    println!("  corpus_name:            {}", m.corpus_name);
    println!("  snapshot_id:            {}", m.snapshot_id);
    println!("  embedding_model:        {}", m.embedding_model);
    println!("  embedding_dimensions:   {}", m.embedding_dimensions);
    println!("  chunk_count:            {}", m.chunk_count);
    println!("  atlas_included:         {}", m.atlas_included);
    if let Some(p) = m.residual_gap_pct {
        println!("  residual_gap_pct:       {p:.2}%");
    }
    if let Some(s) = m.filter_signature.as_deref() {
        println!("  filter_signature:       {s}");
    }
    if let Some(s) = m.canonical_fingerprint.as_deref() {
        println!("  canonical_fingerprint:  {s}");
    }
    if let Some(s) = m.source_recipe_sha256.as_deref() {
        println!("  source_recipe_sha256:   {s}");
    }
    if let Some(n) = m.notes.as_deref() {
        println!("  notes:                  {n}");
    }
    println!("  producer_version:       {}", m.producer_version);
    println!("  created_at:             {} (unix)", m.created_at);
}

// ─── restore ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct RestoreArgs {
    /// HuggingFace ref `<repo>/<filename>` (positional, mutually exclusive with --archive).
    hf_ref: Option<String>,
    archive: Option<PathBuf>,
    as_id: Option<String>,
    into: Option<PathBuf>,
    expected_sha256: Option<String>,
    embedding_model: String,
    embedding_dim: usize,
}

fn parse_restore_args(args: &[String]) -> std::result::Result<RestoreArgs, String> {
    let mut out = RestoreArgs {
        embedding_model: "qwen-embedding-0.6b".to_string(),
        embedding_dim: 1024,
        ..Default::default()
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--archive" => {
                let v = iter.next().ok_or("--archive requires a path")?;
                out.archive = Some(PathBuf::from(v));
            }
            "--as" => {
                let v = iter.next().ok_or("--as requires a corpus id")?;
                out.as_id = Some(v.clone());
            }
            "--into" => {
                let v = iter.next().ok_or("--into requires a path")?;
                out.into = Some(PathBuf::from(v));
            }
            "--expected-sha256" => {
                let v = iter
                    .next()
                    .ok_or("--expected-sha256 requires a hex string")?;
                out.expected_sha256 = Some(v.clone());
            }
            "--embedding-model" => {
                let v = iter.next().ok_or("--embedding-model requires a name")?;
                out.embedding_model = v.clone();
            }
            "--embedding-dim" => {
                let v = iter.next().ok_or("--embedding-dim requires an integer")?;
                out.embedding_dim = v
                    .parse()
                    .map_err(|_| format!("--embedding-dim: '{v}' is not an integer"))?;
            }
            "--help" | "-h" => return Err("__help__".into()),
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            other => {
                if out.hf_ref.is_some() {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                out.hf_ref = Some(other.to_string());
            }
        }
    }
    Ok(out)
}

async fn cmd_restore(args: &[String]) -> i32 {
    let parsed = match parse_restore_args(args) {
        Ok(p) => p,
        Err(msg) if msg == "__help__" => {
            sovereign_cli_shared::help::print(&HELP_SNAPSHOT_RESTORE);
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };

    if parsed.hf_ref.is_some() == parsed.archive.is_some() {
        eprintln!(
            "exactly one of <hf_repo>/<filename> (positional) or --archive <path> is required"
        );
        return 2;
    }

    let sovereign_data_dir = parsed
        .into
        .clone()
        .unwrap_or_else(|| sovereign_root());
    if !sovereign_data_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&sovereign_data_dir) {
            eprintln!("cannot create {}: {e}", sovereign_data_dir.display());
            return 1;
        }
    }

    // Resolve the archive — either local or HF fetch.
    let archive_path = if let Some(local) = parsed.archive.clone() {
        if !local.is_file() {
            eprintln!(
                "--archive {} does not exist or is not a file",
                local.display()
            );
            return 1;
        }
        local
    } else {
        let hf_ref = parsed.hf_ref.as_deref().unwrap();
        let Some((repo, filename)) = split_hf_ref(hf_ref) else {
            eprintln!(
                "expected <hf_repo>/<filename> (e.g. svrnmesh/wikipedia-index/wikipedia-...tar.zst); got: {hf_ref}"
            );
            return 2;
        };
        let url = format!("https://huggingface.co/datasets/{repo}/resolve/main/{filename}");
        let download_dir = sovereign_data_dir.join("snapshots/_downloads");
        if let Err(e) = std::fs::create_dir_all(&download_dir) {
            eprintln!("cannot create {}: {e}", download_dir.display());
            return 1;
        }
        println!("Downloading {url} ...");
        match fetch_hf_archive(&url, &download_dir, &filename).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Download failed: {e}");
                return 1;
            }
        }
    };

    // Peek the manifest so we can default --as to the archive's id and
    // surface the headline stats before the (possibly slow) extract.
    let preview = match read_manifest_from_archive(&archive_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Cannot read manifest from {}: {e}", archive_path.display());
            return 1;
        }
    };
    println!();
    println!("archive manifest:");
    println!("  archive_corpus_id:    {}", preview.corpus_id);
    println!("  snapshot_id:          {}", preview.snapshot_id);
    println!(
        "  embedding_model:      {} ({}d)",
        preview.embedding_model, preview.embedding_dimensions
    );
    println!("  chunk_count:          {}", preview.chunk_count);
    println!("  atlas_included:       {}", preview.atlas_included);
    if let Some(p) = preview.residual_gap_pct {
        println!("  residual_gap_pct:     {p:.2}%");
    }

    let target_id = parsed
        .as_id
        .clone()
        .unwrap_or_else(|| preview.corpus_id.clone());
    println!();
    if target_id != preview.corpus_id {
        println!(
            "Extracting under target corpus id: {target_id} (renaming from {})",
            preview.corpus_id
        );
    } else {
        println!("Extracting under archive's corpus id: {target_id}");
    }

    let expected_sha = parsed.expected_sha256.as_deref();
    if expected_sha.is_none() {
        eprintln!(
            "Note: --expected-sha256 not set. Restore will run without integrity verification; \
             for the production path always pass the sha256 from the recipe's [prebuilt] block."
        );
    }

    let outcome = match restore_snapshot_archive(
        &archive_path,
        &sovereign_data_dir,
        &target_id,
        expected_sha,
        &parsed.embedding_model,
        parsed.embedding_dim,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Restore failed: {e}");
            return 1;
        }
    };

    println!();
    println!("✓ Restored");
    println!("  index_dir:    {}", outcome.index_dir.display());
    if let Some(p) = outcome.enrichment_dir.as_ref() {
        println!("  enrichment:   {}", p.display());
    }
    if !outcome.manifest.bundled_corpora.is_empty() {
        // Cheap sanity check: every advertised sibling should now exist
        // on disk. Warn-on-miss rather than fail because the primary
        // restore succeeded; the operator may want to recover the
        // missing piece without redoing the multi-GB pull.
        let siblings_root = sovereign_data_dir.join("indexes");
        let mut missing: Vec<&String> = Vec::new();
        for id in &outcome.manifest.bundled_corpora {
            if !siblings_root.join(id).is_dir() {
                missing.push(id);
            }
        }
        if missing.is_empty() {
            println!(
                "  siblings:     {} corpus/corpora restored under {}/",
                outcome.manifest.bundled_corpora.len(),
                siblings_root.display()
            );
        } else {
            eprintln!(
                "  ⚠ {} of {} bundled siblings did not land on disk; first missing: {}",
                missing.len(),
                outcome.manifest.bundled_corpora.len(),
                missing.first().map(|s| s.as_str()).unwrap_or("?")
            );
        }
    }
    println!(
        "  bytes:        {} ({:.2} GB)",
        outcome.archive_size_bytes,
        outcome.archive_size_bytes as f64 / 1.073e9_f64
    );
    println!();
    println!("Next: `svrn corpus diag {target_id}` to confirm chunk count + L5 coverage.");
    0
}

/// Split a HuggingFace ref of the form `<org>/<dataset>/<filename>`
/// into `(repo, filename)`. Returns None if the ref has fewer than
/// three slash-separated components (we always need at least
/// org/dataset/file). Trailing slashes are tolerated; filename can
/// contain slashes too if the user uploaded into a subdirectory.
fn split_hf_ref(s: &str) -> Option<(String, String)> {
    // HF repo ids are exactly "<org>/<name>" — first two segments.
    // Everything after is the path-in-repo.
    let mut parts = s.splitn(3, '/');
    let org = parts.next()?;
    let name = parts.next()?;
    let filename = parts.next()?;
    if org.is_empty() || name.is_empty() || filename.is_empty() {
        return None;
    }
    Some((format!("{org}/{name}"), filename.to_string()))
}

/// Download `url` to `<download_dir>/<filename>` with HTTP Range
/// resume and bounded retries. Streams to `<dest>.part`; renames to
/// `<dest>` on completion. On a partial download (network drop,
/// `stream: error decoding response body`, etc.) the partial bytes
/// are kept and the next attempt continues from the existing offset
/// via `Range: bytes=<existing_len>-`.
///
/// Retries up to `max_attempts` times with exponential backoff,
/// mirroring the upload retry policy. The total wall time is bounded
/// by `attempts × per_attempt_timeout`.
async fn fetch_hf_archive(
    url: &str,
    download_dir: &Path,
    filename: &str,
) -> std::result::Result<PathBuf, String> {
    let max_attempts: u32 = 5;
    let dest = download_dir.join(filename);
    if dest.exists() {
        // Already fully downloaded on a prior run — sha verify inside
        // restore is the integrity gate.
        return Ok(dest);
    }
    let part = download_dir.join(format!("{filename}.part"));
    let mut last_err = String::new();
    for attempt in 1..=max_attempts {
        if attempt > 1 {
            let backoff_secs = (15u64 * (1 << (attempt - 2))).min(300);
            let resume_offset = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            eprintln!(
                "Attempt {attempt}/{max_attempts}: waiting {backoff_secs}s before resuming \
                 from {:.2} GB (previous error: {last_err}) …",
                resume_offset as f64 / 1.073e9_f64
            );
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
        }
        match fetch_hf_archive_attempt(url, &part).await {
            Ok(()) => {
                std::fs::rename(&part, &dest)
                    .map_err(|e| format!("rename {} -> {}: {e}", part.display(), dest.display()))?;
                return Ok(dest);
            }
            Err(msg) => {
                last_err = msg;
            }
        }
    }
    Err(format!(
        "download failed after {max_attempts} attempts: {last_err}"
    ))
}

/// One attempt at the download. Uses `Range: bytes=<existing>-` when
/// `<dest>.part` already has bytes from a prior interrupted attempt,
/// and appends to the same file. Returns Ok when the stream ends
/// cleanly with the expected total size, Err otherwise — leaving the
/// `.part` file in place for the next retry to pick up.
async fn fetch_hf_archive_attempt(url: &str, part: &Path) -> std::result::Result<(), String> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let existing_len: u64 = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);

    let client = reqwest::Client::builder()
        .user_agent("sovereign-cli snapshot-restore")
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let mut req = client.get(url);
    if existing_len > 0 {
        req = req.header("Range", format!("bytes={existing_len}-"));
    }
    // Private/gated HF datasets require a bearer token. Same env-var
    // convention used by the model-download path
    // (sovereign-inference/src/setup_planner.rs).
    if let Ok(tok) = std::env::var("HF_TOKEN") {
        if !tok.is_empty() {
            req = req.bearer_auth(tok);
        }
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    // 200 OK = fresh full body; 206 Partial Content = Range honoured.
    // Anything else (including 416 Range Not Satisfiable when the
    // server already has all bytes — rare for HF but possible) is a
    // hard error for this attempt; the retry loop will try again.
    if existing_len > 0 && status == reqwest::StatusCode::OK {
        // Server ignored Range (returns full body) — restart from 0
        // by truncating the existing .part.
        eprintln!("  server ignored Range; restarting download from 0");
        if let Some(parent) = part.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::File::create(part).map_err(|e| format!("truncate {}: {e}", part.display()))?;
    } else if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(format!("{url} returned {status}"));
    }

    let content_length = resp.content_length();
    let total_expected = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        // For 206, content_length is the remaining bytes — total is
        // existing + remaining.
        content_length.map(|c| existing_len + c)
    } else {
        content_length
    };

    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(part)
        .await
        .map_err(|e| format!("open {} for append: {e}", part.display()))?;
    // If status was 200 OK with existing_len > 0, we truncated above —
    // re-open in write mode to ensure offset is 0.
    let mut written: u64 = if status == reqwest::StatusCode::OK {
        out.set_len(0).await.ok();
        0
    } else {
        existing_len
    };

    let mut stream = resp.bytes_stream();
    let mut last_logged: u64 = written;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream: {e}"))?;
        out.write_all(&chunk)
            .await
            .map_err(|e| format!("write: {e}"))?;
        written += chunk.len() as u64;
        if written - last_logged > (1 << 28) {
            // ~256 MiB tick.
            if let Some(total) = total_expected {
                eprintln!(
                    "  downloaded {:.2} / {:.2} GB",
                    written as f64 / 1.073e9_f64,
                    total as f64 / 1.073e9_f64
                );
            } else {
                eprintln!("  downloaded {:.2} GB", written as f64 / 1.073e9_f64);
            }
            last_logged = written;
        }
    }
    out.flush().await.map_err(|e| format!("flush: {e}"))?;
    drop(out);

    // Validate length matches Content-Length when the server told us.
    if let Some(expected) = total_expected {
        if written != expected {
            return Err(format!(
                "incomplete stream: wrote {written} bytes, expected {expected}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_dir(sections: &[(&str, &[u64])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<String> = sections
            .iter()
            .map(|(id, ids)| {
                format!(
                    r#"{{"id":"{id}","title":"{id}","first_line":"","word_count":1,"chunk_ids":{ids:?}}}"#
                )
            })
            .collect();
        std::fs::write(
            dir.path().join("chapters.json"),
            format!(
                r#"{{"corpus_id":"c","schema_version":1,"chapters":[{}]}}"#,
                rows.join(",")
            ),
        )
        .unwrap();
        dir
    }

    /// The case the gate exists for: SEP siblings shipping with sections and
    /// no join, which a downloader can never repair.
    #[test]
    fn a_sibling_with_sections_and_no_join_fails_the_gate() {
        let primary = manifest_dir(&[("sec_0001", &[1, 2])]);
        let sib = manifest_dir(&[("sec_0001", &[]), ("sec_0002", &[])]);
        let audit = audit_section_joins(
            "sep",
            primary.path(),
            &[("sep-abduction".into(), sib.path().to_path_buf())],
        );
        assert_eq!(audit.checked, 2);
        assert_eq!(audit.joined, 1);
        assert_eq!(audit.unjoined, vec![("sep-abduction".to_string(), 2)]);
    }

    /// A corpus with no chapter manifest has nothing to join and must not be
    /// dragged into the refusal — most bundles are exactly this shape.
    #[test]
    fn a_corpus_without_sections_is_not_a_defect() {
        let primary = tempfile::tempdir().unwrap();
        let audit = audit_section_joins("plain", primary.path(), &[]);
        assert_eq!(audit.checked, 0);
        assert!(audit.unjoined.is_empty());
    }

    /// A section that genuinely contains no chunk is legitimate; only a
    /// corpus that joins NOTHING is a fault.
    #[test]
    fn a_partial_join_passes_the_gate() {
        let dir = manifest_dir(&[("sec_0001", &[7]), ("sec_0002", &[])]);
        let audit = audit_section_joins("c", dir.path(), &[]);
        assert_eq!(audit.joined, 1);
        assert!(audit.unjoined.is_empty(), "a partial join must not block a publish");
    }

    /// The primary corpus is audited too, not just the siblings.
    #[test]
    fn the_primary_corpus_is_gated_as_well() {
        let primary = manifest_dir(&[("sec_0001", &[])]);
        let audit = audit_section_joins("solo", primary.path(), &[]);
        assert_eq!(audit.unjoined, vec![("solo".to_string(), 1)]);
    }

    #[test]
    fn the_override_flag_parses_and_defaults_off() {
        let base = parse_publish_args(&["c".to_string()]).unwrap();
        assert!(!base.allow_unjoined_sections, "the gate must be on by default");
        let overridden = parse_publish_args(&[
            "c".to_string(),
            "--allow-unjoined-sections".to_string(),
        ])
        .unwrap();
        assert!(overridden.allow_unjoined_sections);
    }
}
