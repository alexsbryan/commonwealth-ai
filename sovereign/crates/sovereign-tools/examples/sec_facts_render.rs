// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sec_facts_render` — the I/O shell around the pure decider
//! (`sovereign_tools::sec_facts_render::render`).
//!
//! This exists for `scripts/setup-sec-corpus.sh`, the repo-side corpus
//! builder, which used to shell out to `scripts/sec_facts.py render`.
//! It is deliberately an EXAMPLE and not a product CLI verb: the
//! decider's real consumer is M2's `sec_edgar` acquirer, which calls
//! `render` in-process, and the shell script is the legacy path that the
//! ticker-driven install supersedes. Minting a `svrn` verb for it would
//! add a product surface with a scheduled death.
//!
//! Usage:
//!   sec_facts_render --map <concept-map.toml> --facts <companyfacts.json> \
//!                    --out <dir> [--ticker AAPL] [--fy 2025 …]
//!   sec_facts_render --map <…> --facts <…> --ask revenue --period FY2025
//!
//! `--ask` is the single-question mode the Python carried: one
//! `(concept, period)` in, a cited figure or a refusal out. Exit 3 on a
//! refusal, so a shell can branch on it.
//!
//! Glassbox: run with `RUST_LOG=sec_facts_render=debug` to get the trace
//! that names every alias fired, every candidate scan, every restatement
//! supersession and every refusal — the replacement for the Python's
//! `--debug` stderr stream (`render_debug.log`).

use std::path::PathBuf;

use sovereign_tools::sec_facts_render::{render, resolve, ConceptMap, RenderRequest, Resolution};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::process::exit(run()?)
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("sec_facts_render=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut map: Option<PathBuf> = None;
    let mut facts: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut ticker: Option<String> = None;
    let mut fys: Vec<i32> = Vec::new();
    let mut ask: Option<String> = None;
    let mut period: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut next = || args.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--map" => map = Some(PathBuf::from(next()?)),
            "--facts" => facts = Some(PathBuf::from(next()?)),
            "--out" => out = Some(PathBuf::from(next()?)),
            "--ticker" => ticker = Some(next()?),
            "--fy" => fys.push(next()?.parse::<i32>()?),
            "--ask" => ask = Some(next()?),
            "--period" => period = Some(next()?),
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    let (Some(map), Some(facts)) = (map, facts) else {
        return Err("--map and --facts are required".into());
    };

    let cmap = ConceptMap::from_toml(&std::fs::read_to_string(&map)?)?;
    let companyfacts: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&facts)?)?;

    if let Some(concept) = ask {
        let period = period.ok_or("--ask needs --period")?;
        return match resolve(&cmap, &companyfacts, &concept, &period)? {
            Resolution::Fact(f) => {
                println!(
                    "{} — {} [{}]: {} {}, {}; source {} accession {} filed {}",
                    f.entity,
                    f.label,
                    f.tag,
                    f.value,
                    f.unit,
                    f.basis,
                    f.form.as_deref().unwrap_or("?"),
                    f.accession.as_deref().unwrap_or("?"),
                    f.filed.as_deref().unwrap_or("?"),
                );
                Ok(0)
            }
            // A refusal is a first-class result, not an error: it exits
            // non-zero so a caller can branch, and it says WHY.
            Resolution::Refused(r) => {
                println!("REFUSED: {}", r.reason);
                Ok(3)
            }
        };
    }

    let out = out.ok_or("--out is required when rendering")?;

    let rendered = render(RenderRequest {
        companyfacts: &companyfacts,
        concept_map: &cmap,
        ticker: ticker.as_deref(),
        fiscal_years: if fys.is_empty() { None } else { Some(&fys) },
    })?;

    // File placement belongs to the caller — that is the seam that keeps
    // `render` pure.
    std::fs::create_dir_all(&out)?;
    for (name, body) in &rendered.fact_files {
        std::fs::write(out.join(name), body)?;
    }
    std::fs::write(
        out.join("_unmapped_concepts.json"),
        serde_json::to_string_pretty(&rendered.unmapped)?,
    )?;
    let facts_written = match &rendered.sidecar {
        Some(store) => {
            std::fs::write(
                out.join("sec_facts.json"),
                serde_json::to_string_pretty(store)?,
            )?;
            store
                .concepts
                .values()
                .map(|c| c.facts.len())
                .sum::<usize>()
        }
        // Absence is REPORTED, never defaulted to an empty store
        // (ARCH §18.3) — and it is a hard failure for a corpus build.
        None => {
            return Err(format!(
                "no concept resolved from {}: NO typed sidecar written",
                facts.display()
            )
            .into())
        }
    };

    println!(
        "rendered {} concept files to {}; {}/{} filer tags unmapped (named in \
         _unmapped_concepts.json); typed sidecar sec_facts.json ({facts_written} facts across {} \
         concepts)",
        rendered.fact_files.len(),
        out.display(),
        rendered.unmapped.unmapped.len(),
        rendered.unmapped.filer_tags_total,
        rendered.sidecar.as_ref().map_or(0, |s| s.concepts.len()),
    );
    Ok(0)
}
