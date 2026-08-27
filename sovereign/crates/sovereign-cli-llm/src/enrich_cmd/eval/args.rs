// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich eval` argument parsing — flags to a typed request, and the
//! refusals that keep a malformed invocation from scoring anything.

// The eval surface is ONE cooperating unit split for size, not a set of
// independent modules: the golden schema, the snapshot, the match primitives
// and the scorers all name each other's types. `use super::*` keeps that one
// import surface in `mod.rs` rather than duplicating it eight ways.
use super::*;

// ── Argument parsing ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PhaseFilter {
    All,
    Positions,
    Atoms,
    Edges,
    FaultLines,
    Gaps,
    Configurations,
}

impl PhaseFilter {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s {
            "all" => Ok(Self::All),
            "positions" | "skeleton" => Ok(Self::Positions),
            "atoms" => Ok(Self::Atoms),
            "edges" => Ok(Self::Edges),
            "fault-lines" | "fault_lines" | "tensions" => Ok(Self::FaultLines),
            "gaps" | "open-questions" | "open_questions" => Ok(Self::Gaps),
            "configurations" | "config" => Ok(Self::Configurations),
            other => Err(format!(
                "unknown --phase: {other:?} (allowed: positions, atoms, edges, fault-lines, gaps, configurations, all)"
            )),
        }
    }

    pub(super) fn includes(self, other: PhaseFilter) -> bool {
        self == Self::All || self == other
    }
}

#[derive(Debug)]
pub(super) struct ParsedEval {
    pub(super) corpus_id: String,
    pub(super) golden_path: PathBuf,
    pub(super) phase: PhaseFilter,
    pub(super) report_path: Option<PathBuf>,
}

pub(super) fn parse_args(args: &[String]) -> Result<ParsedEval, String> {
    let mut corpus_id: Option<String> = None;
    let mut golden_path: Option<PathBuf> = None;
    let mut phase = PhaseFilter::All;
    let mut report_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--phase" => {
                let raw = args
                    .get(i + 1)
                    .ok_or("--phase requires a value".to_string())?;
                phase = PhaseFilter::parse(raw)?;
                i += 2;
            }
            "--report" => {
                report_path = Some(PathBuf::from(
                    args.get(i + 1)
                        .ok_or("--report requires a path".to_string())?,
                ));
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag: {other}"));
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if golden_path.is_none() {
                    golden_path = Some(PathBuf::from(other));
                } else {
                    return Err(format!("unexpected positional argument: {other}"));
                }
                i += 1;
            }
        }
    }
    Ok(ParsedEval {
        corpus_id: corpus_id.ok_or_else(|| "missing <corpus-id>".to_string())?,
        golden_path: golden_path.ok_or_else(|| "missing <golden-set-path>".to_string())?,
        phase,
        report_path,
    })
}
