//! Closed enum of canonical primitives. Adding a variant requires
//! touching every other module that matches on `Primitive` — that
//! is intentional: the convergence test for "does this primitive
//! close a class?" lives in PR review, not in a wiki.

use serde::{Deserialize, Serialize};

/// Identity of a canonical primitive — used in telemetry, registry
/// keys, and adapter equivalence tests. Smaller than `Primitive`
/// (which carries args) so it round-trips through JSON cleanly as
/// an identifier alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveKind {
    InspectWorkdir,
    WriteFile,
    /// Replace a contiguous range of lines in an existing file.
    /// Smaller than WriteFile (less JSON-escape pressure on the
    /// `content` field) and bounded in scope — added 2026-05-23
    /// after the python 3.2 sweep showed full-file rewrites
    /// occasionally letting prose-in-source leak through the
    /// JSON-string emit boundary. The pre-write syntax check runs
    /// on the FULL post-patch content, so syntactically-broken
    /// patches are rejected at the write boundary same as
    /// write_file.
    PatchFile,
    /// Language-agnostic build step. The command is bound at
    /// `ExecCtx.build_cmd` per problem — Rust uses
    /// `cargo build 2>&1`, Go uses `go build ./...`, Python is a
    /// no-op. Per the multi-language plan: the bench was always
    /// intended for Rust + Go + TS + Python; the primitive holds
    /// the verb, the problem config holds the command.
    Build,
    /// Language-agnostic smoke test. Bound at `ExecCtx.verify_cmd`.
    Smoke,
    AgentDone,
    /// Planner emits this to deliver a chunked plan to the
    /// Implementer. Closes the orientation-vs-execution attention
    /// split.
    AgentPlan,
    /// Implementer signals "ready for verify, hand off to the
    /// Evaluator."
    HandoffToEvaluator,
    /// Evaluator signals "verification failed / needs another
    /// pass" with a diagnosis the Implementer threads into its
    /// next turn.
    HandoffToImplementer,
}

impl PrimitiveKind {
    /// Stable identifier used in tool descriptors and trace events.
    /// The model sees this string as the tool name in the OpenAI
    /// chat completion `tools` array.
    pub const fn id(&self) -> &'static str {
        match self {
            PrimitiveKind::InspectWorkdir => "inspect_workdir",
            PrimitiveKind::WriteFile => "write_file",
            PrimitiveKind::PatchFile => "patch_file",
            PrimitiveKind::Build => "build",
            PrimitiveKind::Smoke => "smoke",
            PrimitiveKind::AgentDone => "agent_done",
            PrimitiveKind::AgentPlan => "agent_plan",
            PrimitiveKind::HandoffToEvaluator => "handoff_to_evaluator",
            PrimitiveKind::HandoffToImplementer => "handoff_to_implementer",
        }
    }

    /// Map a tool-call name back to its canonical kind. Returns
    /// `None` for any unknown string — callers (adapters) decide
    /// whether to translate from agent-specific aliases first.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "inspect_workdir" => Some(PrimitiveKind::InspectWorkdir),
            "write_file" => Some(PrimitiveKind::WriteFile),
            "patch_file" => Some(PrimitiveKind::PatchFile),
            "build" => Some(PrimitiveKind::Build),
            "smoke" => Some(PrimitiveKind::Smoke),
            "agent_done" => Some(PrimitiveKind::AgentDone),
            "agent_plan" => Some(PrimitiveKind::AgentPlan),
            "handoff_to_evaluator" => Some(PrimitiveKind::HandoffToEvaluator),
            "handoff_to_implementer" => Some(PrimitiveKind::HandoffToImplementer),
            _ => None,
        }
    }

    /// Exhaustive list of all canonical primitives. Used by the
    /// registry seeder and the adapter equivalence tests.
    pub const fn all() -> &'static [PrimitiveKind] {
        &[
            PrimitiveKind::InspectWorkdir,
            PrimitiveKind::WriteFile,
            PrimitiveKind::PatchFile,
            PrimitiveKind::Build,
            PrimitiveKind::Smoke,
            PrimitiveKind::AgentDone,
            PrimitiveKind::AgentPlan,
            PrimitiveKind::HandoffToEvaluator,
            PrimitiveKind::HandoffToImplementer,
        ]
    }
}

/// Parsed primitive invocation — kind + typed args. Adapter
/// `translate` methods produce one of these from an agent-specific
/// tool call; the registry dispatches the matching executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "args", rename_all = "snake_case")]
pub enum Primitive {
    InspectWorkdir(InspectIntent),
    WriteFile(WriteFileArgs),
    PatchFile(PatchFileArgs),
    Build,
    Smoke(SmokeArgs),
    AgentDone(AgentDoneArgs),
    AgentPlan(AgentPlanArgs),
    HandoffToEvaluator(HandoffToEvaluatorArgs),
    HandoffToImplementer(HandoffToImplementerArgs),
}

impl Primitive {
    pub const fn kind(&self) -> PrimitiveKind {
        match self {
            Primitive::InspectWorkdir(_) => PrimitiveKind::InspectWorkdir,
            Primitive::WriteFile(_) => PrimitiveKind::WriteFile,
            Primitive::PatchFile(_) => PrimitiveKind::PatchFile,
            Primitive::Build => PrimitiveKind::Build,
            Primitive::Smoke(_) => PrimitiveKind::Smoke,
            Primitive::AgentDone(_) => PrimitiveKind::AgentDone,
            Primitive::AgentPlan(_) => PrimitiveKind::AgentPlan,
            Primitive::HandoffToEvaluator(_) => PrimitiveKind::HandoffToEvaluator,
            Primitive::HandoffToImplementer(_) => PrimitiveKind::HandoffToImplementer,
        }
    }
}

/// Polymorphic argument shape for `inspect_workdir`. Collapses pi's
/// `read` + `ls` + `find` + `grep` into one canonical primitive (per
/// the architecture plan §"Essential primitives" — read-state is
/// one class, the underlying syscall differs by intent only).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectIntent {
    /// Read the file at `path` (workdir-relative).
    File { path: String },
    /// List entries in directory at `path`.
    Dir { path: String },
    /// Find files under `root` whose name matches `pattern`
    /// (substring match, not regex).
    FindByName { root: String, pattern: String },
    /// Grep file contents under `root` for `pattern` (substring
    /// match, line-oriented).
    GrepContents { root: String, pattern: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileArgs {
    pub path: String,
    pub content: String,
}

/// Arguments for `patch_file`: replace a contiguous range of lines
/// in an existing file with `new_content`.
///
/// Lines are 1-indexed and inclusive: `start_line=5, end_line=7`
/// replaces lines 5, 6, 7. `start_line == end_line` replaces a
/// single line. `new_content` may be multi-line (split on `\n`) or
/// empty (deletes the range). The post-patch full content is
/// syntax-checked at the write boundary just like write_file.
///
/// Out of scope for v1: no insert-without-replace (use write_file
/// for net-new files; for in-place insertion replace the
/// neighboring line and include it verbatim in new_content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchFileArgs {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub new_content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmokeArgs {
    /// Optional test-name filter. None runs the whole bound suite.
    /// The per-language test runner (cargo / go test / pytest /
    /// vitest) interprets the filter according to its convention.
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDoneArgs {
    /// Free-form reason. Recorded in telemetry; not used for
    /// control flow.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPlanArgs {
    /// 3-6 sentence plan from the Planner. Sticky into the
    /// `RoleDossier.plan` field — every Implementer + Evaluator
    /// call sees it for the rest of the run.
    pub plan: String,
    /// Optional list of files the plan intends to create (not yet
    /// in the workdir). Hints Implementer about net-new vs.
    /// modification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_to_create: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffToEvaluatorArgs {
    /// Implementer's one-line summary of what changed this turn.
    /// Threaded into the Evaluator's dossier.
    pub what_you_changed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffToImplementerArgs {
    /// Evaluator's diagnosis of what the verification revealed.
    /// Implementer reads this in its next system message.
    pub diagnosis: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_ids_round_trip() {
        for kind in PrimitiveKind::all() {
            let id = kind.id();
            assert_eq!(PrimitiveKind::from_id(id), Some(*kind));
        }
    }

    #[test]
    fn unknown_id_returns_none() {
        assert!(PrimitiveKind::from_id("bash").is_none());
        assert!(PrimitiveKind::from_id("").is_none());
    }

    #[test]
    fn primitive_kind_round_trips() {
        for kind in PrimitiveKind::all() {
            let p = match kind {
                PrimitiveKind::InspectWorkdir => Primitive::InspectWorkdir(InspectIntent::File {
                    path: "x".into(),
                }),
                PrimitiveKind::WriteFile => Primitive::WriteFile(WriteFileArgs {
                    path: "x".into(),
                    content: String::new(),
                }),
                PrimitiveKind::PatchFile => Primitive::PatchFile(PatchFileArgs {
                    path: "x".into(),
                    start_line: 1,
                    end_line: 1,
                    new_content: String::new(),
                }),
                PrimitiveKind::Build => Primitive::Build,
                PrimitiveKind::Smoke => Primitive::Smoke(SmokeArgs::default()),
                PrimitiveKind::AgentDone => Primitive::AgentDone(AgentDoneArgs {
                    reason: "test".into(),
                }),
                PrimitiveKind::AgentPlan => Primitive::AgentPlan(AgentPlanArgs {
                    plan: "test plan".into(),
                    files_to_create: None,
                }),
                PrimitiveKind::HandoffToEvaluator => {
                    Primitive::HandoffToEvaluator(HandoffToEvaluatorArgs {
                        what_you_changed: "wrote lib.rs".into(),
                    })
                }
                PrimitiveKind::HandoffToImplementer => {
                    Primitive::HandoffToImplementer(HandoffToImplementerArgs {
                        diagnosis: "build failed".into(),
                    })
                }
            };
            assert_eq!(p.kind(), *kind);
        }
    }
}
