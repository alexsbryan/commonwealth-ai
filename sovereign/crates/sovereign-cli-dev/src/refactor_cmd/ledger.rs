// SPDX-License-Identifier: AGPL-3.0-or-later
//! The join — live sites from the detectors, durable judgements from git.
//!
//! ```text
//!   Holding  =  Site (measured, this run)  ⋈  Label (durable, in git)
//! ```
//!
//! Neither half is a database. The sites come from instruments that run now;
//! the labels come from append-only files a human can read in a PR. The join
//! happens in memory in microseconds, because the whole population is a few
//! thousand rows — the sweep is what costs seconds, never the bookkeeping.
//!
//! # The one number
//!
//! `open` is holdings whose label says `converge` and whose detector still
//! fires. It falls when a site is genuinely converged and cannot be moved any
//! other way, because **nothing in this program writes progress** — see
//! [`super::detector`] for why that is the whole design rather than a detail.
//!
//! An unlabelled site is reported as unlabelled, not counted as work and not
//! silently dropped. That is the difference between "we have not adjudicated
//! this yet" and "there is nothing here", and collapsing the two is exactly the
//! absence-reported-as-a-default failure ARCH §18.3 names.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use corpus_engine_scip::converge::SourceScope;
use corpus_engine_scip::ScipGraph;
use kernel_types::Judgement;

use super::detector::{self, CostClass, DetectorCtx, DetectorId, Site};
use super::labels::{Label, LabelStore};

/// One site, joined to whatever the ledger knows about it.
#[derive(Debug, Clone)]
pub struct Holding {
    pub site: Site,
    pub label: Option<Label>,
}

impl Holding {
    /// Is this open work? Only an explicit `converge` counts.
    pub fn is_open_work(&self) -> bool {
        self.label.as_ref().is_some_and(|l| l.disp.is_work())
    }

    pub fn destination(&self) -> &str {
        self.label.as_ref().map_or("", |l| l.dest.as_str())
    }
}

/// One detector's contribution, with the verdict that says whether to believe
/// it.
#[derive(Debug)]
pub struct DetectorSlice {
    pub id: DetectorId,
    pub control: Judgement,
    pub settings_digest: String,
    pub holdings: Vec<Holding>,
    /// Set when the detector itself failed to run.
    pub error: Option<String>,
    /// Set when the detector was not run because it exceeds the close budget.
    /// A skipped detector is REPORTED as skipped — never omitted, and never
    /// folded into the count as a zero (ARCH §18.3).
    pub skipped: Option<&'static str>,
}

impl DetectorSlice {
    pub fn is_live(&self) -> bool {
        self.error.is_none()
            && self.skipped.is_none()
            && self.control.verdict() == kernel_types::Verdict::Passed
    }
}

/// The whole picture, as of this instant.
#[derive(Debug)]
pub struct Ledger {
    pub slices: Vec<DetectorSlice>,
    /// Labels that no longer join to any live site.
    ///
    /// The key survives a line move, a reformat, and an unrelated edit — but
    /// NOT a file rename or a symbol rename, which is the one way a judgement
    /// can be lost. Carried as the actual keys, not a count, because a number
    /// tells you something was lost and a key tells you what to re-label.
    pub orphans: Vec<String>,
    /// Label lines that would not parse, named with file and line.
    pub malformed: Vec<String>,
    /// The commit the SCIP graph was built from. A number that does not name
    /// the tree it was measured on is not a measurement.
    pub graph_commit: String,
}

impl Ledger {
    /// Open holdings per destination — the burn-down.
    pub fn by_destination(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for slice in self.slices.iter().filter(|s| s.is_live()) {
            for h in slice.holdings.iter().filter(|h| h.is_open_work()) {
                let dest = if h.destination().is_empty() {
                    "(no destination declared)"
                } else {
                    h.destination()
                };
                *out.entry(dest.to_string()).or_insert(0) += 1;
            }
        }
        out
    }

    /// The one number. Only counts slices whose control fired.
    pub fn open(&self) -> usize {
        self.slices
            .iter()
            .filter(|s| s.is_live())
            .flat_map(|s| s.holdings.iter())
            .filter(|h| h.is_open_work())
            .count()
    }
}

/// The graph, read once. Owning it separately lets `next` and `close` build a
/// [`DetectorCtx`] without duplicating the ~7-11s load or the borrow dance.
pub struct GraphData {
    pub symbols: Vec<corpus_engine_scip::ScipSymbolRecord>,
    pub refs: Vec<corpus_engine_scip::ScipRefRecord>,
    pub scope: SourceScope,
    pub commit: String,
}

pub async fn load_graph(index_path: &Path, corpus_id: &str) -> Result<GraphData, String> {
    let db_path = index_path.join("scip_graph.db");
    let graph = ScipGraph::open(&db_path, corpus_id)
        .map_err(|e| format!("opening {}: {e}", db_path.display()))?;
    let symbols = graph
        .iter_all_symbols()
        .await
        .map_err(|e| format!("reading symbols: {e}"))?;
    let refs = graph
        .iter_all_refs()
        .await
        .map_err(|e| format!("reading refs: {e}"))?;
    let commit = graph
        .last_indexed_head()
        .await
        .map(|h| h.chars().take(12).collect())
        .unwrap_or_else(|| "unknown".to_string());
    Ok(GraphData {
        symbols,
        refs,
        scope: SourceScope::default(),
        commit,
    })
}

impl GraphData {
    pub fn ctx<'a>(
        &'a self,
        root: &'a Path,
        index_path: &'a Path,
        corpus_id: &'a str,
    ) -> DetectorCtx<'a> {
        DetectorCtx {
            root,
            symbols: &self.symbols,
            refs: &self.refs,
            scope: &self.scope,
            index_path,
            corpus_id,
        }
    }
}

/// Load the graph once, run every detector, join to the labels.
pub async fn build(
    root: &Path,
    index_path: &Path,
    corpus_id: &str,
    include_expensive: bool,
) -> Result<Ledger, String> {
    let db_path = index_path.join("scip_graph.db");
    let graph = ScipGraph::open(&db_path, corpus_id)
        .map_err(|e| format!("opening {}: {e}", db_path.display()))?;

    // Read both tables ONCE and share them across all five detectors — this is
    // the expensive part (~7-11s, ~0.8-1.1 GB), and paying it per detector
    // would be five times the wrong answer.
    let symbols = graph
        .iter_all_symbols()
        .await
        .map_err(|e| format!("reading symbols: {e}"))?;
    let refs = graph
        .iter_all_refs()
        .await
        .map_err(|e| format!("reading refs: {e}"))?;
    let scope = SourceScope::default();
    // The graph's OWN accessor, not a second SELECT against scip_meta — the
    // one decider for "which commit is this graph" (ARCH §10.6).
    let graph_commit = graph
        .last_indexed_head()
        .await
        .map(|h| h.chars().take(12).collect())
        .unwrap_or_else(|| "unknown".to_string());

    let ctx = DetectorCtx {
        root,
        symbols: &symbols,
        refs: &refs,
        scope: &scope,
        index_path,
        corpus_id,
    };

    let store = LabelStore::load(root);
    let mut slices = Vec::new();
    let mut all_sites: Vec<Site> = Vec::new();

    for d in detector::all() {
        if let (CostClass::Expensive(basis), false) = (d.cost(), include_expensive) {
            slices.push(DetectorSlice {
                id: d.id(),
                control: Judgement::never_ran(
                    format!("{} control", d.id().as_str()),
                    kernel_types::Reason::literal("detector not run — exceeds the close budget"),
                ),
                settings_digest: d.settings_digest(),
                holdings: Vec::new(),
                error: None,
                skipped: Some(basis),
            });
            continue;
        }
        match d.fire(&ctx).await {
            Ok(report) => {
                all_sites.extend(report.sites.iter().cloned());
                let holdings = report
                    .sites
                    .into_iter()
                    .map(|site| {
                        let label = store.get(&site.key()).cloned();
                        Holding { site, label }
                    })
                    .collect();
                slices.push(DetectorSlice {
                    id: report.detector,
                    control: report.control,
                    settings_digest: report.settings_digest,
                    holdings,
                    error: None,
                    skipped: None,
                });
            }
            Err(e) => slices.push(DetectorSlice {
                id: d.id(),
                // A detector that could not run has NOT judged anything. It is
                // never-ran, and never-ran is not a pass (ARCH §18.2).
                control: Judgement::never_ran(
                    format!("{} control", d.id().as_str()),
                    kernel_types::Reason::new(e.clone())
                        .unwrap_or_else(|| kernel_types::Reason::literal("detector failed to run")),
                ),
                settings_digest: d.settings_digest(),
                holdings: Vec::new(),
                error: Some(e),
                skipped: None,
            }),
        }
    }

    let orphans: Vec<String> = store
        .orphans(&all_sites)
        .into_iter()
        .map(|l| format!("{} -> {} ({})", l.key, l.dest, l.disp.as_str()))
        .collect();
    Ok(Ledger {
        slices,
        orphans,
        malformed: store.malformed,
        graph_commit,
    })
}

pub fn render(ledger: &Ledger) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "═══════════════════════════════════════════════════════════════"
    );
    let _ = writeln!(out, " refactor ledger — open holdings");
    let _ = writeln!(
        out,
        "═══════════════════════════════════════════════════════════════"
    );
    let _ = writeln!(out, " graph commit: {}", ledger.graph_commit);
    let _ = writeln!(out);

    // Detectors first, because a number from an instrument whose control went
    // quiet must not be read as a number at all.
    let _ = writeln!(out, " DETECTORS");
    for s in &ledger.slices {
        let verdict = if let Some(basis) = s.skipped {
            format!("SKIPPED (expensive: {basis}) — pass --all to include")
        } else if let Some(e) = &s.error {
            format!("NEVER-RAN — {e}")
        } else {
            match s.control.verdict() {
                kernel_types::Verdict::Passed => format!("live ({} sites)", s.holdings.len()),
                _ => format!("COULD-NOT-JUDGE — {}", s.control.reason().as_str()),
            }
        };
        let _ = writeln!(out, "   {:<12} {}", s.id.as_str(), verdict);
        let _ = writeln!(out, "   {:<12}   settings: {}", "", s.settings_digest);
    }
    let _ = writeln!(out);

    let by_dest = ledger.by_destination();
    if by_dest.is_empty() {
        let _ = writeln!(out, " No open holdings from any live detector.");
        let _ = writeln!(
            out,
            " That is a starting state, not a clean bill: run `label` to adjudicate."
        );
    } else {
        let _ = writeln!(out, " OPEN BY DESTINATION");
        for (dest, n) in &by_dest {
            let _ = writeln!(out, "   {n:>6}  {dest}");
        }
    }
    let _ = writeln!(out);

    // Everything the join could not account for, named.
    let unlabelled: usize = ledger
        .slices
        .iter()
        .filter(|s| s.is_live())
        .flat_map(|s| s.holdings.iter())
        .filter(|h| h.label.is_none())
        .count();
    let _ = writeln!(out, " open (converge-labelled):  {}", ledger.open());
    let _ = writeln!(out, " unlabelled sites:          {unlabelled}");
    let _ = writeln!(out, " orphaned labels:           {}", ledger.orphans.len());
    if !ledger.orphans.is_empty() {
        // A key only breaks on a file or symbol RENAME. Name them, so the
        // judgement can be moved rather than silently re-adjudicated.
        let _ = writeln!(
            out,
            "   (a label's key survives a line move; these lost a file or symbol rename)"
        );
        for o in ledger.orphans.iter().take(10) {
            let _ = writeln!(out, "   {o}");
        }
    }
    if !ledger.malformed.is_empty() {
        let _ = writeln!(
            out,
            " MALFORMED label lines:     {}",
            ledger.malformed.len()
        );
        for m in ledger.malformed.iter().take(5) {
            let _ = writeln!(out, "   {m}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::labels::Disposition;
    use super::*;

    fn site(detector: DetectorId, file: &str, token: &str) -> Site {
        Site {
            detector,
            file: file.to_string(),
            line: 1,
            locus: file.to_string(),
            token: token.to_string(),
            note: String::new(),
        }
    }

    fn label(key: &str, dest: &str, disp: Disposition) -> Label {
        Label {
            key: key.to_string(),
            dest: dest.to_string(),
            disp,
            why: "t".into(),
            by: "seat".into(),
            at: "2026-08-23".into(),
        }
    }

    fn slice(id: DetectorId, control: Judgement, holdings: Vec<Holding>) -> DetectorSlice {
        DetectorSlice {
            id,
            control,
            settings_digest: "t".into(),
            holdings,
            error: None,
            skipped: None,
        }
    }

    fn live() -> Judgement {
        Judgement::passed("t", kernel_types::Reason::literal("control fired"))
    }

    fn dead() -> Judgement {
        Judgement::could_not_judge("t", kernel_types::Reason::literal("control silent"))
    }

    fn ledger(slices: Vec<DetectorSlice>) -> Ledger {
        Ledger {
            slices,
            orphans: Vec::new(),
            malformed: Vec::new(),
            graph_commit: "abc123".into(),
        }
    }

    #[test]
    fn only_converge_labelled_sites_are_open_work() {
        let s = site(DetectorId::Name, "a.rs", "Verdict");
        let holdings = vec![
            Holding {
                label: Some(label(
                    &s.key(),
                    "kernel_types::Verdict",
                    Disposition::Converge,
                )),
                site: s.clone(),
            },
            Holding {
                label: Some(label("other", "", Disposition::Distinct)),
                site: site(DetectorId::Name, "b.rs", "Verdict"),
            },
            // Unlabelled: real, reported, but not counted as work.
            Holding {
                label: None,
                site: site(DetectorId::Name, "c.rs", "Verdict"),
            },
        ];
        let l = ledger(vec![slice(DetectorId::Name, live(), holdings)]);
        assert_eq!(l.open(), 1);
    }

    /// The interlock, at the ledger level: a detector whose control went silent
    /// contributes NOTHING to the number — neither a count nor a zero that
    /// could be mistaken for progress.
    #[test]
    fn a_dead_detectors_holdings_never_reach_the_number() {
        let s = site(DetectorId::Name, "a.rs", "Verdict");
        let holdings = vec![Holding {
            label: Some(label(
                &s.key(),
                "kernel_types::Verdict",
                Disposition::Converge,
            )),
            site: s,
        }];
        let l = ledger(vec![slice(DetectorId::Name, dead(), holdings)]);
        assert_eq!(l.open(), 0, "a silent control must not contribute a count");
        assert!(l.by_destination().is_empty());
    }

    #[test]
    fn a_detector_that_failed_to_run_is_never_ran_not_zero() {
        let s = DetectorSlice {
            id: DetectorId::Behaviour,
            control: Judgement::never_ran("t", kernel_types::Reason::literal("no index")),
            settings_digest: "t".into(),
            holdings: Vec::new(),
            error: Some("no index".into()),
            skipped: None,
        };
        assert!(!s.is_live());
        let out = render(&ledger(vec![s]));
        assert!(out.contains("NEVER-RAN"), "{out}");
    }

    #[test]
    fn destinations_aggregate_the_open_count() {
        let a = site(DetectorId::Name, "a.rs", "Verdict");
        let b = site(DetectorId::Name, "b.rs", "Verdict");
        let holdings = vec![
            Holding {
                label: Some(label(
                    &a.key(),
                    "kernel_types::Verdict",
                    Disposition::Converge,
                )),
                site: a,
            },
            Holding {
                label: Some(label(
                    &b.key(),
                    "kernel_types::Verdict",
                    Disposition::Converge,
                )),
                site: b,
            },
        ];
        let l = ledger(vec![slice(DetectorId::Name, live(), holdings)]);
        assert_eq!(l.by_destination()["kernel_types::Verdict"], 2);
    }

    /// An empty ledger must not print as success — it prints as a starting
    /// state with the next action named.
    #[test]
    fn an_empty_ledger_does_not_read_as_a_clean_bill() {
        let out = render(&ledger(vec![slice(DetectorId::Name, live(), Vec::new())]));
        assert!(out.contains("not a clean bill"), "{out}");
    }

    #[test]
    fn the_render_always_names_the_graph_commit_it_measured() {
        let out = render(&ledger(Vec::new()));
        assert!(out.contains("abc123"), "{out}");
    }

    #[test]
    fn malformed_label_lines_are_surfaced_in_the_report() {
        let mut l = ledger(Vec::new());
        l.malformed.push("labels/name.jsonl:4: bad".into());
        let out = render(&l);
        assert!(out.contains("MALFORMED"), "{out}");
    }
}
