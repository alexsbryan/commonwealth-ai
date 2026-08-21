// SPDX-License-Identifier: AGPL-3.0-or-later
//! drb1-r3a measurement-leg harness (order drb1-r3a; landed by order
//! drb1-r3b item 3 as the campaign's re-measure instrument — §18.4:
//! the instrument ships with the result).
//!
//! Re-renders recorded deep-research runs through the REAL production
//! renderers — `render_report` (called by the loop's `finish`,
//! deep_research/mod.rs) and `render_race` (called by the CLI's
//! `write_race_render`, deep_research_cmd.rs) — from each run's
//! recorded artifacts, and re-runs the R3a citation-registry pass
//! (`final_claims`) to capture orphan-citation WARNs. Nothing here
//! re-implements a render decision (one decider, one name): the
//! outputs are the production functions' outputs over the recorded
//! verdict sets.
//!
//! Inputs, per run dir:
//!   charter.json            -> question (render_report's `question`)
//!   report.md               -> the `# ` heading (render_race's question
//!                              source in production)
//!   verdict-set.json        -> claims + empty_rounds
//!   manifest.json           -> reframe / alignment / residue
//!   evidence-window-*.json  -> merged per `merge_windows`' rule
//!                              (deep_research/mod.rs:1415: dedup by
//!                              source_url, first wins, capped at the
//!                              charter cap). That method is
//!                              &mut-self private; its rule is
//!                              reproduced here verbatim — a NAMED
//!                              substitution (§18.3). Only `chunks` is
//!                              read by the registry pass.
//!
//! Outputs, alongside the originals: by default `rescan-report.md`
//! and `rescan-render-race.md`; with `--graded`,
//! `rescan-graded-report.md` and `rescan-graded-render-race.md` (the
//! drb1-r3b re-measure: the un-suffixed files stay as the "before"),
//! plus a JSON metrics summary on stdout. The summary counts the
//! pages the production renderers emitted — open-question markers,
//! graded [single-origin] rows, Findings / Open-questions section
//! bytes — and applies the SAME counters to any pre-existing
//! un-suffixed rescan pages for the before/after pair.
//!
//! The `final_claims` reconstruction feeds each recorded FinalClaim's
//! `evidence_ids` back through the registry as `supporting_chunk_ids`
//! (with placeholder action/witness — the registry reads neither). Its
//! recomputed FLAGS are discarded: the renderers above consume the
//! recorded verdict-set rows, whose flags came from the flight's real
//! audits. Only the citation resolution and its orphan WARNs are
//! measured from this pass.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sovereign_core::deep_research::audit::ClaimAudit;
use sovereign_core::deep_research::icd::{
    EvidenceWindow, GateAction, Manifest, VerdictSet, WindowChunk,
};
use sovereign_core::deep_research::render::{final_claims, render_race, render_report};

fn main() {
    let mut graded = false;
    let dirs: Vec<PathBuf> = std::env::args()
        .skip(1)
        .filter(|a| {
            if a == "--graded" {
                graded = true;
                false
            } else {
                true
            }
        })
        .map(PathBuf::from)
        .collect();
    if dirs.is_empty() {
        eprintln!("usage: rescan-render [--graded] <run-dir>...");
        std::process::exit(2);
    }
    for dir in &dirs {
        if let Err(e) = rescan(dir, graded) {
            eprintln!("rescan failed for {}: {e}", dir.display());
            std::process::exit(1);
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

/// The render-page metrics the drb1-race campaign measures. Counted
/// over the markdown the production renderers emitted — the artifact
/// itself, never a re-derivation.
#[derive(serde::Serialize)]
struct PageMetrics {
    open_markers: usize,
    graded_rows: usize,
    findings_bytes: usize,
    open_questions_bytes: usize,
}

fn page_metrics(page: &str) -> PageMetrics {
    let open_markers = page
        .lines()
        .filter(|l| l.contains("**[could-not-judge]**") || l.contains("**[open question]**"))
        .count();
    let graded_rows = page
        .lines()
        .filter(|l| l.starts_with("- **[single-origin]**"))
        .count();
    PageMetrics {
        open_markers,
        graded_rows,
        findings_bytes: section_bytes(page, "Findings").unwrap_or(0),
        open_questions_bytes: section_bytes(page, "Open questions").unwrap_or(0),
    }
}

fn section_bytes(page: &str, heading: &str) -> Option<usize> {
    let start = page.find(&format!("## {heading}"))?;
    let rest = &page[start..];
    let end = rest[3..]
        .find("\n## ")
        .map(|i| i + 3 + 1)
        .unwrap_or(rest.len());
    Some(end)
}

fn metrics_value(m: &PageMetrics, claims: usize) -> serde_json::Value {
    serde_json::json!({
        "open_markers": m.open_markers,
        "graded_rows": m.graded_rows,
        "findings_bytes": m.findings_bytes,
        "open_questions_bytes": m.open_questions_bytes,
        "marker_fraction": if claims > 0 {
            m.open_markers as f64 / claims as f64
        } else {
            f64::NAN
        },
    })
}

fn rescan(dir: &Path, graded: bool) -> Result<(), String> {
    let charter: serde_json::Value = read_json(&dir.join("charter.json"))?;
    let question = charter["question"]
        .as_str()
        .ok_or("charter carries no question")?
        .to_string();
    let cap = charter["charter"]["evidence_window_max_chunks"]
        .as_u64()
        .ok_or("charter carries no evidence_window_max_chunks")? as usize;
    let vs: VerdictSet = read_json(&dir.join("verdict-set.json"))?;
    let manifest: Manifest = read_json(&dir.join("manifest.json"))?;

    // render_race's question source in production (write_race_render):
    // the report's first `# ` heading.
    let old_report =
        std::fs::read_to_string(dir.join("report.md")).map_err(|e| format!("report.md: {e}"))?;
    let race_question = old_report
        .lines()
        .find_map(|l| l.strip_prefix("# ").map(str::to_string))
        .ok_or("report.md carries no `# ` heading")?;
    if race_question != question {
        // Named, never silently substituted (§18.3).
        println!(
            "note {}: charter question != report.md `# ` heading; render_race uses the heading (production parity)",
            vs.run_id
        );
    }

    // The merged window, per merge_windows (deep_research/mod.rs:1415).
    let mut window_paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("evidence-window-"))
                .unwrap_or(false)
        })
        .collect();
    window_paths.sort();
    let windows: Vec<EvidenceWindow> = window_paths
        .iter()
        .map(|p| read_json(p))
        .collect::<Result<_, _>>()?;
    let mut seen: HashSet<String> = HashSet::new();
    let mut chunks: Vec<WindowChunk> = Vec::new();
    let mut capped_dropped = 0usize;
    for w in &windows {
        for c in &w.chunks {
            if !seen.insert(c.source_url.clone()) {
                continue;
            }
            if chunks.len() >= cap {
                capped_dropped += 1;
                continue;
            }
            chunks.push(c.clone());
        }
    }
    let merged = EvidenceWindow {
        icd: "evidence_window".to_string(),
        version: windows.first().map(|w| w.version).unwrap_or(1),
        run_id: vs.run_id.clone(),
        charter_hash: vs.charter_hash.clone(),
        round: windows.len() as u32,
        chunks,
        // Neither field is read by the registry pass (final_claims reads
        // window.chunks only); named substitution, empty is fine.
        fetch_failures: Vec::new(),
        dedup_refused: Vec::new(),
        derived_custody: windows
            .last()
            .map(|w| w.derived_custody.clone())
            .unwrap_or_default(),
    };

    // The renders — the real production functions over the recorded
    // verdict set, INSIDE the sink so render-time WARNs (the drb1-r3b
    // unknown-flag default) are captured, not dropped on the floor.
    let sink = WarnSink::default();
    let handle = sink.clone();
    let (report, race) = tracing::subscriber::with_default(sink, || {
        (
            render_report(
                &question,
                &vs.claims,
                &vs.run_id,
                manifest.reframe.as_ref(),
                manifest.alignment.as_ref(),
                &manifest.residue,
                &vs.empty_rounds,
            ),
            render_race(&race_question, &vs.claims, &vs.run_id),
        )
    });
    let (report_name, race_name) = if graded {
        ("rescan-graded-report.md", "rescan-graded-render-race.md")
    } else {
        ("rescan-report.md", "rescan-render-race.md")
    };
    std::fs::write(dir.join(report_name), &report)
        .map_err(|e| format!("{report_name} write: {e}"))?;
    std::fs::write(dir.join(race_name), &race).map_err(|e| format!("{race_name} write: {e}"))?;

    // The citation registry pass — capture its orphan WARNs through a
    // scoped subscriber (glassbox: the events the flight would emit).
    let audits: Vec<ClaimAudit> = vs
        .claims
        .iter()
        .map(|c| ClaimAudit {
            claim: c.text.clone(),
            verdict: c.verdict,
            action: GateAction::AbstainedDecline,
            witness: Default::default(),
            supporting_chunk_ids: c.evidence_ids.clone(),
            empty_evidence_window: false,
            reason: None,
            corroboration: c.corroboration.clone(),
        })
        .collect();
    let sink = WarnSink::default();
    let handle2 = sink.clone();
    let rederived = tracing::subscriber::with_default(sink, || final_claims(&audits, &merged));
    let mut events = handle.events.lock().unwrap().clone();
    events.extend(handle2.events.lock().unwrap().iter().cloned());
    let orphan_warns = events
        .iter()
        .filter(|e| e.contains("citation registry"))
        .count();
    let unknown_flag_warns = events
        .iter()
        .filter(|e| e.contains("unknown flag defaults WALLED"))
        .count();
    let cited: usize = rederived.iter().map(|c| c.citations.len()).sum();
    let referenced: usize = vs.claims.iter().map(|c| c.evidence_ids.len()).sum();
    // The registry's citation channel should reproduce the recorded one
    // (pre-R3a final_claims resolved identically — only the WARN is new).
    // ClaimCitation carries no PartialEq — compare through its JSON
    // projection (the recorded channel vs the registry's re-derivation).
    let citations_match_recorded = rederived.iter().zip(vs.claims.iter()).all(|(r, c)| {
        serde_json::to_value(&r.citations).ok() == serde_json::to_value(&c.citations).ok()
    });

    // The metrics — same counters over the fresh pages and over any
    // pre-existing un-suffixed rescan pages (the "before").
    let claims = vs.claims.len();
    let after_report = page_metrics(&report);
    let after_race = page_metrics(&race);
    let before_report = std::fs::read_to_string(dir.join("rescan-report.md"))
        .ok()
        .map(|p| metrics_value(&page_metrics(&p), claims));
    let before_race = std::fs::read_to_string(dir.join("rescan-render-race.md"))
        .ok()
        .map(|p| metrics_value(&page_metrics(&p), claims));

    let summary = serde_json::json!({
        "run_id": vs.run_id,
        "claims": claims,
        "report_file": report_name,
        "race_file": race_name,
        "report": metrics_value(&after_report, claims),
        "race": metrics_value(&after_race, claims),
        "before": {
            "report": before_report,
            "race": before_race,
        },
        "merged_chunks": merged.chunks.len(),
        "cap": cap,
        "capped_dropped": capped_dropped,
        "referenced_chunk_refs": referenced,
        "resolved_citations": cited,
        "orphan_warns": orphan_warns,
        "unknown_flag_warns": unknown_flag_warns,
        "warn_events": events,
        "rederived_citations_match_recorded": citations_match_recorded,
        "rescan_report_bytes": report.len(),
        "rescan_race_bytes": race.len(),
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    Ok(())
}

/// A minimal tracing subscriber that collects WARN-or-worse events on
/// the `deep_research` target — enough to count the citation-registry
/// orphan WARNs and the drb1-r3b unknown-flag WARNs the real code
/// emits.
#[derive(Default, Clone)]
struct WarnSink {
    events: std::sync::Arc<Mutex<Vec<String>>>,
}

impl tracing::Subscriber for WarnSink {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        *meta.level() <= tracing::Level::WARN && meta.target() == "deep_research"
    }

    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct V(Vec<String>);
        impl tracing::field::Visit for V {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push(format!("{}={:?}", field.name(), value));
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.0.push(format!("{}={}", field.name(), value));
            }
        }
        let mut v = V(Vec::new());
        event.record(&mut v);
        self.events
            .lock()
            .unwrap()
            .push(format!("{} {}", event.metadata().level(), v.0.join(" ")));
    }

    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}
