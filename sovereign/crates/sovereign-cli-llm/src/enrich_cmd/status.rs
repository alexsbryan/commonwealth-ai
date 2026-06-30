// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich status <corpus>` — quick per-phase staleness table.

use corpus_engine::enrichment::pipeline::{PhaseCache, PhaseCacheStatus, PipelinePhase};

use super::config::EnrichConfig;
use super::paths;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn enrich status",
    summary: "Print the cache-freshness status of every phase for a corpus.",
    sections: &[HelpSection::Usage("svrn enrich status <corpus-id>")],
};

pub async fn cmd_status(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let corpus_id = match args.first() {
        Some(s) if !s.starts_with("--") => s.clone(),
        _ => {
            eprintln!("error: missing <corpus-id>");
            eprintln!();
            help::print(&HELP);
            return 2;
        }
    };
    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!(
        "  Corpus: {} · pipeline={} · source={}",
        cfg.corpus_id,
        cfg.pipeline_id,
        cfg.source_path.display()
    );
    println!();
    let cache = PhaseCache::new(paths::cache_dir(&corpus_id));
    for phase in PipelinePhase::ALL {
        let status = match cache.status(*phase) {
            Ok(s) => s,
            Err(e) => {
                println!("    {:<18} ERROR ({e})", phase.id());
                continue;
            }
        };
        let symbol = match status {
            PhaseCacheStatus::Fresh => "✓ fresh",
            PhaseCacheStatus::Stale => "⚠ stale",
            PhaseCacheStatus::NeverRun => "· never run",
        };
        println!("    {:<18} {symbol}", phase.id());
    }
    0
}
