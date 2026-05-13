//! `sovereign corpus snapshot` — publish and inspect prebuilt-index
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
    prebuilt_toml_snippet, publish_snapshot, read_manifest_from_archive, PublishOptions,
    SnapshotManifest,
};
use corpus_engine::CorpusIndex;

use crate::util::help::{Help, HelpSection};

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

const PRODUCER_VERSION: &str = concat!("sovereign-cli/", env!("CARGO_PKG_VERSION"));

const HELP_SNAPSHOT: Help = Help {
    command: "sovereign corpus snapshot",
    summary: "Publish and inspect prebuilt-index tarballs for cold-start onboarding.",
    sections: &[
        HelpSection::Usage("sovereign corpus snapshot <subcommand> [args]"),
        HelpSection::Subcommands(&[
            ("publish <id>", "Build a .tar.zst snapshot of an installed corpus and (optionally) upload to HuggingFace"),
            ("inspect <archive>", "Print the manifest from an existing snapshot archive"),
        ]),
        HelpSection::Notes(
            "`publish` writes to ~/.sovereign/snapshots/<filename>.tar.zst by default.\n\
             Including the atlas adds ~2 GB for Wikipedia but is what restorers will expect.",
        ),
    ],
};

const HELP_SNAPSHOT_PUBLISH: Help = Help {
    command: "sovereign corpus snapshot publish",
    summary: "Package an installed corpus into a .tar.zst snapshot for distribution.",
    sections: &[
        HelpSection::Usage("sovereign corpus snapshot publish <corpus_id> [flags]"),
        HelpSection::Flags(&[
            ("--no-atlas", "Skip the enrichment/<id>/ subtree (smaller archive, restorers must re-enrich)"),
            ("--output <path>", "Where to write the archive (default ~/.sovereign/snapshots/<filename>)"),
            ("--snapshot-id <id>", "Override the auto-generated snapshot id (default: <corpus>-<model>-<date>)"),
            ("--notes <text>", "Free-form notes recorded in the manifest"),
            ("--residual-gap-pct <f>", "Known incompleteness percent (e.g. 2.81 for wikipedia)"),
            ("--zstd-level <int>", "Zstd compression level (default 19 — high ratio, slower; use 3 for fast)"),
            ("--upload <repo>", "Upload to HuggingFace via `hf upload` (e.g. svrnmesh/wikipedia-index)"),
            ("--dry-run", "Print the upload command instead of running it"),
        ]),
        HelpSection::Notes(
            "Pre-flight: run `sovereign corpus diag <id>` first to confirm completeness;\n\
             the resulting count goes into the manifest. After publish, paste the\n\
             printed [prebuilt] block into sovereign-recipes/<id>/recipe.toml.",
        ),
    ],
};

const HELP_SNAPSHOT_INSPECT: Help = Help {
    command: "sovereign corpus snapshot inspect",
    summary: "Print the manifest stored at the root of a snapshot archive.",
    sections: &[
        HelpSection::Usage("sovereign corpus snapshot inspect <archive.tar.zst>"),
    ],
};

pub async fn run_snapshot(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h" | "help") {
        crate::util::help::print(&HELP_SNAPSHOT);
        return if args.is_empty() { 1 } else { 0 };
    }
    match args[0].as_str() {
        "publish" => cmd_publish(&args[1..]).await,
        "inspect" => cmd_inspect(&args[1..]),
        other => {
            eprintln!("Unknown snapshot subcommand: {other}");
            crate::util::help::print(&HELP_SNAPSHOT);
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
}

fn parse_publish_args(args: &[String]) -> std::result::Result<PublishArgs, String> {
    let mut out = PublishArgs {
        include_atlas: true,
        zstd_level: 19,
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
    Ok(out)
}

async fn cmd_publish(args: &[String]) -> i32 {
    let parsed = match parse_publish_args(args) {
        Ok(p) => p,
        Err(msg) if msg == "__help__" => {
            crate::util::help::print(&HELP_SNAPSHOT_PUBLISH);
            return 0;
        }
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };

    let Some(corpus_id) = parsed.corpus_id else {
        eprintln!("usage: sovereign corpus snapshot publish <corpus_id> [flags]");
        return 2;
    };

    let index_dir = home_dir().join(".sovereign/indexes").join(&corpus_id);
    if !index_dir.exists() {
        eprintln!("Index directory not found: {}", index_dir.display());
        eprintln!(
            "Install the corpus first with `sovereign corpus install {corpus_id}` or pull a partition."
        );
        return 1;
    }
    let enrichment_root = home_dir().join(".sovereign/enrichment").join(&corpus_id);
    let enrichment_dir = if parsed.include_atlas && enrichment_root.exists() {
        Some(enrichment_root.clone())
    } else {
        None
    };
    if parsed.include_atlas && enrichment_dir.is_none() {
        eprintln!(
            "Note: --no-atlas not set, but {} does not exist — publishing without an atlas subtree.",
            enrichment_root.display()
        );
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
        home_dir()
            .join(".sovereign/snapshots")
            .join(format!("{snapshot_id}.tar.zst"))
    });

    println!("Building archive at {} (zstd level {}) ...", output_path.display(), parsed.zstd_level);
    println!("  this can take several minutes for multi-GB indexes");

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
    };

    let outcome = match publish_snapshot(opts) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Publish failed: {e}");
            let _ = std::fs::remove_file(&output_path);
            return 1;
        }
    };

    println!();
    println!("  archive: {}", outcome.archive_path.display());
    println!("  size:    {:.2} GB ({} bytes)", outcome.archive_size_bytes as f64 / 1.073e9_f64, outcome.archive_size_bytes);
    println!("  sha256:  {}", outcome.archive_sha256);
    println!("  atlas:   {}", if outcome.manifest.atlas_included { "included" } else { "not included" });

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
            println!("Uploading to {target} ...");
            match run_hf_upload(repo, &outcome.archive_path) {
                Ok(()) => println!("Upload complete."),
                Err(msg) => {
                    eprintln!("Upload failed: {msg}");
                    eprintln!("You can retry manually:");
                    eprintln!("  {cmd_str}");
                    return 1;
                }
            }
        }
    }

    println!();
    println!("{}", prebuilt_toml_snippet(&outcome, parsed.upload_repo.as_deref().unwrap_or("svrnmesh/<repo>")));

    0
}

/// Default snapshot id derived from corpus_id + embedding model
/// (read from `_corpus_meta.json`) + today's UTC date.
fn default_snapshot_id(corpus_id: &str, index_dir: &Path) -> String {
    let model = std::fs::read_to_string(index_dir.join("_corpus_meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("embedding_model").and_then(|m| m.as_str()).map(str::to_string))
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

fn cmd_inspect(args: &[String]) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        crate::util::help::print(&HELP_SNAPSHOT_INSPECT);
        return if args.is_empty() { 2 } else { 0 };
    }
    let archive_path = PathBuf::from(&args[0]);
    let manifest = match read_manifest_from_archive(&archive_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to read manifest from {}: {e}", archive_path.display());
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
