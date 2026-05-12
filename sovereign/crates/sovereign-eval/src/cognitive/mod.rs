//! Cognitive unit-test harness — the fast tier of the three-tier
//! evaluation architecture.
//!
//! Each item in `sovereign/inquiries/cognitive/<category>/<id>.toml`
//! probes one bounded competency of the Fast slot in isolation.
//! The suite runs in single-digit minutes and is meant to drive
//! same-day iteration on prompt assembly, slot configuration, and
//! the Fast-slot model itself.
//!
//! Scoring is mechanical — no judge call. The eval brainstorm's
//! discipline applies here: mechanical scoring central, judge bias
//! deferred to the deep tier where the cost is amortized over
//! richer artifacts.

pub mod item;
pub mod report;
pub mod runner;
pub mod scorer;

use anyhow::Result;
use chrono::Utc;
use std::path::{Path, PathBuf};

pub use item::{Category, Item};
pub use report::{BaselineDiff, BuildOpts as ReportBuildOpts, Report};
pub use runner::{ItemResult, RunOpts};
pub use scorer::Outcome;

#[derive(Debug, Clone)]
pub struct SuiteOpts<'a> {
    pub bank_root: &'a Path,
    pub workspace_root: &'a Path,
    pub daemon_url: &'a str,
    pub model: &'a str,
    pub category_filter: Option<Category>,
    pub item_id_filter: Option<&'a str>,
    pub temperature: f32,
    pub seed: u64,
    pub max_tokens: u32,
}

/// Run every item under `bank_root` (optionally filtered) and return
/// the aggregated [`Report`]. The caller decides where to write it.
pub fn run_suite(opts: SuiteOpts<'_>) -> Result<Report> {
    let started_at = Utc::now().to_rfc3339();
    let items = item::load_all(opts.bank_root)?;
    if items.is_empty() {
        anyhow::bail!(
            "no cognitive items found under {}",
            opts.bank_root.display()
        );
    }

    let filtered: Vec<&Item> = items
        .iter()
        .filter(|it| match opts.category_filter {
            Some(c) => it.item.category == c,
            None => true,
        })
        .filter(|it| match opts.item_id_filter {
            Some(id) => it.item.id == id,
            None => true,
        })
        .collect();

    if filtered.is_empty() {
        anyhow::bail!("filters matched zero items");
    }

    tracing::info!(
        count = filtered.len(),
        bank = %opts.bank_root.display(),
        model = %opts.model,
        "running cognitive suite"
    );

    let run_opts = RunOpts {
        daemon_url: opts.daemon_url,
        model: opts.model,
        temperature: opts.temperature,
        seed: opts.seed,
        max_tokens: opts.max_tokens,
        workspace_root: opts.workspace_root,
    };

    let mut outcomes = Vec::with_capacity(filtered.len());
    for (idx, it) in filtered.iter().enumerate() {
        let progress = format!("[{}/{}]", idx + 1, filtered.len());
        match runner::run_item(it, &run_opts) {
            Ok(result) => {
                let outcome = scorer::score(it, &result);
                tracing::info!(
                    "{progress} {} ({}) {} {}ms",
                    outcome.item_id,
                    outcome.category,
                    if outcome.passed { "PASS" } else { "FAIL" },
                    outcome.elapsed_ms
                );
                if !outcome.passed {
                    tracing::debug!(reason = %outcome.reason, "fail reason");
                }
                outcomes.push(outcome);
            }
            Err(e) => {
                tracing::error!(
                    "{progress} {} render failed: {e}",
                    it.item.id
                );
                outcomes.push(Outcome {
                    item_id: it.item.id.clone(),
                    category: it.item.category.as_str().to_string(),
                    passed: false,
                    reason: format!("render error: {e}"),
                    response_raw: String::new(),
                    elapsed_ms: 0,
                    model: opts.model.to_string(),
                });
            }
        }
    }

    let ended_at = Utc::now().to_rfc3339();
    let run_id = format!("cognitive-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    Ok(report::build(
        ReportBuildOpts {
            run_id: &run_id,
            started_at: &started_at,
            ended_at: &ended_at,
            model: opts.model,
            daemon_url: opts.daemon_url,
            temperature: opts.temperature,
            seed: opts.seed,
        },
        outcomes,
    ))
}

/// Default bank root — `<workspace>/sovereign/inquiries/cognitive`.
pub fn default_bank_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("sovereign").join("inquiries").join("cognitive")
}
