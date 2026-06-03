//! `sovereign agent-bench aggregate` — walk one or more artifact roots,
//! derive a `FailureClass` per (cell, problem), and emit a histogram
//! plus per-class repro paths.
//!
//! Input is a directory tree of the shape produced by `run` and
//! `sweep` (which is just a sequence of `run` invocations with
//! different daemon configs):
//!
//! ```text
//! <root>/
//!   <cell-1>/
//!     <problem-1>/
//!       agent.json
//!       witness.json
//!       …
//!     <problem-2>/
//!       …
//!   <cell-2>/
//!     …
//! ```
//!
//! When a single `run` artifact dir is passed (one level less deep —
//! `<root>/<problem>/agent.json`), the walk recognises that shape
//! too. The cell label in the output is the directory's basename.
//!
//! Class derivation is delegated to `failure_class::classify_from_dir`;
//! see that module for the rule table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::failure_class::{classify_from_dir, FailureClass};

#[derive(Debug, Error)]
pub enum AggregateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("--root <path> required (try --help)")]
    MissingRoot,
    #[error("root path `{0}` does not exist or is not a directory")]
    BadRoot(PathBuf),
    #[error("no agent.json files found under {0}")]
    NoArtifacts(PathBuf),
    #[error("unknown flag `{0}` (try --help)")]
    UnknownFlag(String),
}

/// CLI args for the aggregate subcommand. Tiny enough that we
/// hand-roll the parser instead of pulling in clap.
#[derive(Debug, Clone)]
pub struct AggregateArgs {
    pub roots: Vec<PathBuf>,
    /// Optional path to write the histogram JSON. If `None`, the
    /// printer emits the table to stdout only.
    pub json_out: Option<PathBuf>,
    /// When true, print one path-per-cell under each class. Off by
    /// default to keep the table compact.
    pub list_paths: bool,
}

impl AggregateArgs {
    pub fn parse(argv: &[String]) -> Result<Self, AggregateError> {
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut json_out: Option<PathBuf> = None;
        let mut list_paths = false;
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            match a.as_str() {
                "--root" => {
                    let v = argv.get(i + 1).ok_or_else(|| {
                        AggregateError::UnknownFlag("--root expects a value".into())
                    })?;
                    roots.push(PathBuf::from(v));
                    i += 2;
                }
                "--json-out" => {
                    let v = argv.get(i + 1).ok_or_else(|| {
                        AggregateError::UnknownFlag("--json-out expects a value".into())
                    })?;
                    json_out = Some(PathBuf::from(v));
                    i += 2;
                }
                "--list-paths" => {
                    list_paths = true;
                    i += 1;
                }
                "--help" | "-h" => {
                    eprintln!("{}", help_text());
                    std::process::exit(0);
                }
                other => return Err(AggregateError::UnknownFlag(other.to_string())),
            }
        }
        if roots.is_empty() {
            return Err(AggregateError::MissingRoot);
        }
        Ok(Self {
            roots,
            json_out,
            list_paths,
        })
    }
}

fn help_text() -> &'static str {
    r#"sovereign agent-bench aggregate --root <path> [--root <path> …]

Walks each artifact root, classifies every (cell, problem) pair using
the failure-class taxonomy, and emits:
  - a console table grouped by class
  - optionally a JSON histogram (--json-out <path>)

Flags:
  --root <path>     directory containing per-cell subdirs (repeatable)
  --json-out <path> write structured histogram to this path
  --list-paths      print one artifact path per cell under each class
  -h, --help        show this help

The class rules are documented in `failure_class.rs`; the priority
order is solved > partial > exit_reason-driven > zero-tool-call shapes
> tool-call outcomes.
"#
}

/// One (root, cell, problem) → class observation. The aggregator's
/// reduce step buckets these by class.
#[derive(Debug, Clone, Serialize)]
pub struct CellObservation {
    pub root: String,
    pub cell: String,
    pub problem: String,
    pub class: String,
    pub artifact_dir: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ClassBucket {
    pub class: String,
    pub description: String,
    pub is_system_failure: bool,
    pub count: usize,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AggregateReport {
    pub roots: Vec<String>,
    pub total_cells: usize,
    pub system_failure_count: usize,
    pub system_failure_pct: f64,
    pub histogram: Vec<ClassBucket>,
    pub observations: Vec<CellObservation>,
}

pub fn run_command(argv: &[String]) -> Result<(), AggregateError> {
    let args = AggregateArgs::parse(argv)?;
    for r in &args.roots {
        if !r.is_dir() {
            return Err(AggregateError::BadRoot(r.clone()));
        }
    }

    let mut observations: Vec<CellObservation> = Vec::new();
    for root in &args.roots {
        let found = walk_artifact_root(root)?;
        observations.extend(found);
    }
    if observations.is_empty() {
        return Err(AggregateError::NoArtifacts(
            args.roots
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from(".")),
        ));
    }

    let report = build_report(&args.roots, observations);
    print_table(&report, args.list_paths);
    if let Some(path) = &args.json_out {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
        eprintln!("aggregate: wrote {}", path.display());
    }
    Ok(())
}

/// Walk one artifact root looking for `<root>/<cell>/<problem>/agent.json`
/// or `<root>/<problem>/agent.json` (single-cell shape produced by
/// `run` without a sweep wrapper).
pub fn walk_artifact_root(root: &Path) -> Result<Vec<CellObservation>, AggregateError> {
    let root_label = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut out: Vec<CellObservation> = Vec::new();
    for top in read_dir_sorted(root)? {
        let top_path = top.path();
        if !top_path.is_dir() {
            continue;
        }
        // Try the two-level shape: this directory's children are
        // problem dirs with agent.json.
        let direct_agent_json = top_path.join("agent.json").is_file();
        if direct_agent_json {
            // Single-cell shape: <root>/<problem>/agent.json.
            let problem = top_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if let Some(class) = classify_from_dir(&top_path) {
                out.push(CellObservation {
                    root: root_label.clone(),
                    cell: root_label.clone(),
                    problem,
                    class: class.id().to_string(),
                    artifact_dir: top_path.display().to_string(),
                });
            }
            continue;
        }
        // Cell-then-problem shape: <root>/<cell>/<problem>/agent.json.
        let cell_label = top_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        for sub in read_dir_sorted(&top_path)? {
            let sub_path = sub.path();
            if !sub_path.is_dir() {
                continue;
            }
            if !sub_path.join("agent.json").is_file() {
                continue;
            }
            let problem = sub_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if let Some(class) = classify_from_dir(&sub_path) {
                out.push(CellObservation {
                    root: root_label.clone(),
                    cell: cell_label.clone(),
                    problem,
                    class: class.id().to_string(),
                    artifact_dir: sub_path.display().to_string(),
                });
            }
        }
    }
    Ok(out)
}

fn read_dir_sorted(p: &Path) -> Result<Vec<std::fs::DirEntry>, AggregateError> {
    let mut entries: Vec<std::fs::DirEntry> =
        std::fs::read_dir(p)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

fn build_report(roots: &[PathBuf], observations: Vec<CellObservation>) -> AggregateReport {
    let mut by_class: BTreeMap<String, ClassBucket> = BTreeMap::new();
    let total_cells = observations.len();
    for obs in &observations {
        let entry = by_class.entry(obs.class.clone()).or_insert_with(|| {
            let fc = parse_class(&obs.class);
            ClassBucket {
                class: obs.class.clone(),
                description: fc.map(|c| c.description().to_string()).unwrap_or_default(),
                is_system_failure: fc.map(|c| c.is_system_failure()).unwrap_or(false),
                count: 0,
                examples: Vec::new(),
            }
        });
        entry.count += 1;
        if entry.examples.len() < 3 {
            entry.examples.push(obs.artifact_dir.clone());
        }
    }
    // Sort histogram: highest-count first; ties broken by class name
    // for stable output.
    let mut histogram: Vec<ClassBucket> = by_class.into_values().collect();
    histogram.sort_by(|a, b| b.count.cmp(&a.count).then(a.class.cmp(&b.class)));
    let system_failure_count: usize = histogram
        .iter()
        .filter(|b| b.is_system_failure)
        .map(|b| b.count)
        .sum();
    let system_failure_pct = if total_cells == 0 {
        0.0
    } else {
        (system_failure_count as f64 / total_cells as f64) * 100.0
    };
    AggregateReport {
        roots: roots.iter().map(|p| p.display().to_string()).collect(),
        total_cells,
        system_failure_count,
        system_failure_pct,
        histogram,
        observations,
    }
}

/// Reverse of `FailureClass::id`. Returns None when the class string
/// doesn't match — keeps the aggregator robust against schema drift
/// without needing the enum to expose a TryFrom<&str>.
fn parse_class(s: &str) -> Option<FailureClass> {
    use FailureClass::*;
    Some(match s {
        "solved" => Solved,
        "partial" => Partial,
        "hung" => Hung,
        "agent_crash" => AgentCrash,
        "token_budget" => TokenBudget,
        "loop_trap" => LoopTrap,
        "tool_denied" => ToolDenied,
        "parse_failed_envelope" => ParseFailedEnvelope,
        "daemon_truncate" => DaemonTruncate,
        "model_chatted" => ModelChatted,
        "empty_response" => EmptyResponse,
        "tool_call_noop" => ToolCallNoop,
        "algorithmic_wrong" => AlgorithmicWrong,
        _ => return None,
    })
}

fn print_table(report: &AggregateReport, list_paths: bool) {
    println!(
        "agent-bench aggregate — roots={} total_cells={} system_failure={}/{} ({:.1}%)",
        report.roots.len(),
        report.total_cells,
        report.system_failure_count,
        report.total_cells,
        report.system_failure_pct
    );
    println!();
    println!(
        "  {:<24} {:>6}  {:<8}  description",
        "class", "count", "kind"
    );
    println!("  {}", "-".repeat(80));
    for bucket in &report.histogram {
        let kind = if bucket.is_system_failure {
            "system"
        } else {
            "model"
        };
        println!(
            "  {:<24} {:>6}  {:<8}  {}",
            bucket.class, bucket.count, kind, bucket.description
        );
    }
    if list_paths {
        println!();
        println!("per-class examples (up to 3):");
        for bucket in &report.histogram {
            println!("  [{}]", bucket.class);
            for ex in &bucket.examples {
                println!("    {ex}");
            }
        }
    }
    let solved = report
        .histogram
        .iter()
        .find(|b| b.class == "solved")
        .map(|b| b.count)
        .unwrap_or(0);
    let partial = report
        .histogram
        .iter()
        .find(|b| b.class == "partial")
        .map(|b| b.count)
        .unwrap_or(0);
    let model_failure_count = report.total_cells - report.system_failure_count - solved - partial;
    println!();
    println!(
        "  funnel: solved={} partial={} model_failure={} system_failure={}",
        solved, partial, model_failure_count, report.system_failure_count
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_cell(root: &Path, cell: &str, problem: &str, agent_json: &str, witness_json: &str) {
        let dir = root.join(cell).join(problem);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("agent.json"), agent_json).unwrap();
        fs::write(dir.join("witness.json"), witness_json).unwrap();
    }

    #[test]
    fn walks_two_level_shape_and_classifies() {
        let tmp = tempfile::tempdir().unwrap();
        // Cell 1: model chatted, no tool call.
        write_cell(
            tmp.path(),
            "coder-noforce",
            "3.2-lights-out",
            r#"{"tokens_input":0,"tokens_output":450,"exit_reason":{"kind":"completed"},"tool_calls":[],"final_assistant_text":"Here is my plan ..."}"#,
            r#"{"pass_fraction":0.0,"passed":0,"failed":12,"total":12,"verify_exit_ok":false}"#,
        );
        // Cell 2: loop trap.
        write_cell(
            tmp.path(),
            "coder-force",
            "3.2-lights-out",
            r#"{"tokens_input":0,"tokens_output":245,"exit_reason":{"kind":"no_progress","detail":{"consecutive_tool_calls":8,"threshold":8}},"tool_calls":[{"turn":2,"tool":"read","args_preview":"","ok":true}],"final_assistant_text":""}"#,
            r#"{"pass_fraction":0.0,"passed":0,"failed":12,"total":12,"verify_exit_ok":false}"#,
        );
        let obs = walk_artifact_root(tmp.path()).unwrap();
        assert_eq!(obs.len(), 2);
        let classes: Vec<&str> = obs.iter().map(|o| o.class.as_str()).collect();
        assert!(classes.contains(&"model_chatted"));
        assert!(classes.contains(&"loop_trap"));
    }

    #[test]
    fn walks_single_cell_shape() {
        let tmp = tempfile::tempdir().unwrap();
        // Single-cell layout: <root>/<problem>/agent.json directly.
        let dir = tmp.path().join("3.2-lights-out");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("agent.json"),
            r#"{"tokens_input":0,"tokens_output":800,"exit_reason":{"kind":"completed"},"tool_calls":[{"turn":2,"tool":"write","args_preview":"src/lib.rs","ok":true}],"final_assistant_text":"DONE"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("witness.json"),
            r#"{"pass_fraction":0.0,"passed":0,"failed":12,"total":12,"verify_exit_ok":false}"#,
        )
        .unwrap();
        let obs = walk_artifact_root(tmp.path()).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].class, "algorithmic_wrong");
    }

    #[test]
    fn report_funnel_arithmetic_holds() {
        let obs = vec![
            mk_obs("a", "p1", "solved"),
            mk_obs("a", "p2", "loop_trap"),
            mk_obs("b", "p1", "model_chatted"),
            mk_obs("b", "p2", "algorithmic_wrong"),
        ];
        let report = build_report(&[PathBuf::from("/x")], obs);
        assert_eq!(report.total_cells, 4);
        // loop_trap is system; model_chatted + algorithmic_wrong are model;
        // solved is success.
        assert_eq!(report.system_failure_count, 1);
        let solved = report
            .histogram
            .iter()
            .find(|b| b.class == "solved")
            .unwrap()
            .count;
        assert_eq!(solved, 1);
    }

    fn mk_obs(cell: &str, problem: &str, class: &str) -> CellObservation {
        CellObservation {
            root: "test".into(),
            cell: cell.into(),
            problem: problem.into(),
            class: class.into(),
            artifact_dir: format!("/x/{cell}/{problem}"),
        }
    }
}
