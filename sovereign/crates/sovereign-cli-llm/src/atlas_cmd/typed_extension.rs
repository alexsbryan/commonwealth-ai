// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas typed-extension <corpus>` — operator surface for
//! the tiered typed-extension pass over RAPTOR cluster summaries.
//!
//! Spec: `sovereign/docs/specs/TYPED_EXTENSION_PASS.md`.
//!
//! Normally the pass runs inside `FolderTieredProvider::finalize_corpus`
//! at the tail of every tiered enrichment build (and per-doc incremental
//! re-enrich). This subcommand exists to:
//!
//! - Re-run extraction against an already-built corpus without a full
//!   re-ingest (useful after a typed-extension prompt iteration).
//! - Run the pass on a corpus whose atoms.json predates the typed-
//!   extension wire-up (e.g. obsidian-vault on the v1→v2 transition).
//! - Smoke-test the orchestrator end-to-end against a running daemon
//!   from a one-line CLI invocation.
//!
//! Idempotent via the `atoms.meta.json` sidecar — a second invocation
//! with no upstream changes prints `status=SkippedManifestMatch` and
//! makes zero LLM calls.

use std::path::PathBuf;
use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;
use sovereign_store::sqlite::SqliteStateStore;
use sovereign_tools::typed_extension::{run_typed_extension, ExtractionStatus};

/// Daemon endpoint default. Overridable via `--endpoint <url>` so
/// peer-daemon or worker-pod invocations are possible from the same
/// CLI surface.
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9741/v1";

/// Context window the OpenAI-shape RemoteApiProvider claims. The
/// typed-extension prompts cap at ~8K decode tokens; the daemon's
/// real model context is whatever the loaded slot reports. This is
/// just an advertised ceiling for the provider's capabilities surface.
const ADVERTISED_CONTEXT: u32 = 32_768;

pub async fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            print_help();
            return 2;
        }
    };
    if parsed.help {
        print_help();
        return 0;
    }

    // Resolve data dir → state db + atlas dir for this corpus.
    let data_dir = match resolve_data_dir() {
        Some(d) => d,
        None => {
            eprintln!("error: cannot resolve data_dir (set HOME or SOVEREIGN_DATA_DIR)");
            return 1;
        }
    };
    let db_path = data_dir.join("sovereign.db");
    let indexes_dir = data_dir.join("indexes");
    // Accept a display name or unique fragment, not just the raw id —
    // ids carry a hash suffix nobody should have to type.
    let corpus_id = match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, &parsed.corpus_id)
    {
        Ok(id) => id,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };
    let corpus_dir = match find_corpus_index_path(&indexes_dir, &corpus_id) {
        Some(p) => p,
        None => {
            eprintln!(
                "error: no installed index for corpus '{corpus_id}' under {}",
                indexes_dir.display()
            );
            return 1;
        }
    };
    let atlas_dir = corpus_dir.join("atlas");

    eprintln!("corpus:    {corpus_id}");
    eprintln!("state db:  {}", db_path.display());
    eprintln!("atlas dir: {}", atlas_dir.display());
    eprintln!("endpoint:  {}", parsed.endpoint);
    eprintln!();

    // Open the daemon's state DB read+write — the typed-extension pass
    // only reads (conv_skeletons / conv_raptor_nodes / vault_themes)
    // but SqliteStateStore::open opens read/write.
    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("error: open {}: {e}", db_path.display());
            return 1;
        }
    };

    // Build a RemoteApiProvider against the daemon. model_id="" so the
    // daemon's OICP picker routes by latency_class (the pass requests
    // Speed::Slow per spec for precision).
    let inference: Arc<dyn InferenceProvider> = Arc::new(RemoteApiProvider::new(
        &parsed.endpoint,
        None,
        "",
        ADVERTISED_CONTEXT,
    ));

    // --force: drop the idempotency manifest so the pass re-extracts
    // even when RAPTOR/theme hashes are unchanged. The operator use
    // cases are prompt iteration and RUN-VARIANCE estimation (same
    // code + same inputs, fresh sampling) — extraction is stochastic
    // and ±1 axis moves on the bench are inside single-run noise.
    if parsed.force {
        let manifest = atlas_dir.join(sovereign_tools::typed_extension::MANIFEST_FILENAME);
        match std::fs::remove_file(&manifest) {
            Ok(()) => eprintln!("--force: removed {}", manifest.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                eprintln!("error: --force could not remove {}: {e}", manifest.display());
                return 1;
            }
        }
    }

    let started = std::time::Instant::now();
    let report = match run_typed_extension(&corpus_id, &store, &inference, &atlas_dir).await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: typed extension failed: {e}");
            return 1;
        }
    };
    let elapsed_ms = started.elapsed().as_millis();

    println!();
    println!("  status:         {:?}", report.status);
    println!("  pass A calls:   {}", report.pass_a_calls);
    println!("  pass B calls:   {}", report.pass_b_calls);
    println!("  elapsed:        {}ms", elapsed_ms);
    if !report.atoms_per_kind.is_empty() {
        println!("  atoms per kind:");
        // Stable render order matches the AXIS_CATALOG argumentative
        // entries (see corpus-engine/src/enrichment/atlas/axis_catalog.rs).
        for key in [
            "mechanism",
            "named_position",
            "evidence",
            "opposition",
            "concession",
        ] {
            let count = report.atoms_per_kind.get(key).copied().unwrap_or(0);
            println!("    {:<16} {}", key, count);
        }
    }
    if !report.soft_failures.is_empty() {
        println!(
            "  soft failures:  {} (extraction still wrote what succeeded)",
            report.soft_failures.len()
        );
        for failure in report.soft_failures.iter().take(5) {
            println!("    · {failure}");
        }
        if report.soft_failures.len() > 5 {
            println!("    · …and {} more", report.soft_failures.len() - 5);
        }
    }

    match report.status {
        ExtractionStatus::Wrote => 0,
        ExtractionStatus::WroteEmpty => {
            eprintln!(
                "  ⚠ wrote zero atoms across all five axes — confirm the corpus has \
                 RAPTOR leaves and the daemon model produces argumentative content"
            );
            0
        }
        ExtractionStatus::SkippedManifestMatch => 0,
        ExtractionStatus::SkippedNoInputs => {
            eprintln!(
                "  ⚠ no leaves or themes for this corpus — run `svrn enrich build` \
                 with the tiered pipeline first, or confirm the corpus has Ready conv_skeletons"
            );
            0
        }
    }
}

struct Args {
    corpus_id: String,
    endpoint: String,
    help: bool,
    force: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut corpus_id: Option<String> = None;
    let mut endpoint = DEFAULT_ENDPOINT.to_string();
    let mut help = false;
    let mut force = false;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--help" | "-h" => help = true,
            "--force" => force = true,
            "--endpoint" => match iter.next() {
                Some(v) => endpoint = v.clone(),
                None => return Err("--endpoint requires a URL".into()),
            },
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_some() {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                corpus_id = Some(other.to_string());
            }
        }
    }
    Ok(Args {
        corpus_id: corpus_id.unwrap_or_default(),
        endpoint,
        help,
        force,
    })
}

fn print_help() {
    println!(
        "svrn atlas typed-extension <corpus> [--endpoint <url>]\n\
         \n\
         Run the tiered typed-extension LLM pass over an already-built corpus's\n\
         RAPTOR leaves + vault_themes. Writes atoms.json + atoms.meta.json into\n\
         the corpus's atlas/ dir.\n\
         \n\
         <corpus>          Corpus id (must already be installed in the indexes dir).\n\
         --endpoint <url>  Daemon OpenAI-shape endpoint. Default: {DEFAULT_ENDPOINT}\n\
         --force           Drop the idempotency manifest first — re-extract with\n\
                           unchanged inputs (prompt iteration / run-variance checks).\n\
         \n\
         Idempotent: re-running with no upstream changes prints SkippedManifestMatch\n\
         and makes zero LLM calls. Triggers Slow-slot decode on the daemon (per spec\n\
         §'Open design questions' #3).",
    );
}

fn resolve_data_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOVEREIGN_DATA_DIR") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".sovereign"))
}

fn find_corpus_index_path(indexes_dir: &std::path::Path, corpus_id: &str) -> Option<PathBuf> {
    // Canonical path: <indexes>/<corpus_id>. Accept either modern
    // (`_corpus_meta.json` present) or legacy (literary-built corpora
    // without the modern install meta file but with an `atlas/`
    // directory the typed-extension pass can write into).
    let canonical = indexes_dir.join(corpus_id);
    if canonical.is_dir()
        && (canonical.join("_corpus_meta.json").is_file() || canonical.join("atlas").is_dir())
    {
        return Some(canonical);
    }
    let entries = std::fs::read_dir(indexes_dir).ok()?;
    let prefix = format!("{corpus_id}-partition-");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&prefix) && !name.contains('.') {
            return Some(entry.path());
        }
    }
    None
}
