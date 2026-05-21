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
    CargoBuild,
    CargoSmoke,
    AgentDone,
}

impl PrimitiveKind {
    /// Stable identifier used in tool descriptors and trace events.
    /// The model sees this string as the tool name in the OpenAI
    /// chat completion `tools` array.
    pub const fn id(&self) -> &'static str {
        match self {
            PrimitiveKind::InspectWorkdir => "inspect_workdir",
            PrimitiveKind::WriteFile => "write_file",
            PrimitiveKind::CargoBuild => "cargo_build",
            PrimitiveKind::CargoSmoke => "cargo_smoke",
            PrimitiveKind::AgentDone => "agent_done",
        }
    }

    /// Map a tool-call name back to its canonical kind. Returns
    /// `None` for any unknown string — callers (adapters) decide
    /// whether to translate from agent-specific aliases first.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "inspect_workdir" => Some(PrimitiveKind::InspectWorkdir),
            "write_file" => Some(PrimitiveKind::WriteFile),
            "cargo_build" => Some(PrimitiveKind::CargoBuild),
            "cargo_smoke" => Some(PrimitiveKind::CargoSmoke),
            "agent_done" => Some(PrimitiveKind::AgentDone),
            _ => None,
        }
    }

    /// Exhaustive list of all canonical primitives. Used by the
    /// registry seeder and the adapter equivalence tests.
    pub const fn all() -> &'static [PrimitiveKind] {
        &[
            PrimitiveKind::InspectWorkdir,
            PrimitiveKind::WriteFile,
            PrimitiveKind::CargoBuild,
            PrimitiveKind::CargoSmoke,
            PrimitiveKind::AgentDone,
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
    CargoBuild,
    CargoSmoke(CargoSmokeArgs),
    AgentDone(AgentDoneArgs),
}

impl Primitive {
    pub const fn kind(&self) -> PrimitiveKind {
        match self {
            Primitive::InspectWorkdir(_) => PrimitiveKind::InspectWorkdir,
            Primitive::WriteFile(_) => PrimitiveKind::WriteFile,
            Primitive::CargoBuild => PrimitiveKind::CargoBuild,
            Primitive::CargoSmoke(_) => PrimitiveKind::CargoSmoke,
            Primitive::AgentDone(_) => PrimitiveKind::AgentDone,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CargoSmokeArgs {
    /// Optional test-name filter passed to `cargo test`. None runs
    /// the whole integration suite.
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDoneArgs {
    /// Free-form reason. Recorded in telemetry; not used for
    /// control flow.
    pub reason: String,
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
            // Each variant has a default-constructible Primitive
            // shape; this test pins that the kind() projection is
            // total.
            let p = match kind {
                PrimitiveKind::InspectWorkdir => Primitive::InspectWorkdir(InspectIntent::File {
                    path: "x".into(),
                }),
                PrimitiveKind::WriteFile => Primitive::WriteFile(WriteFileArgs {
                    path: "x".into(),
                    content: String::new(),
                }),
                PrimitiveKind::CargoBuild => Primitive::CargoBuild,
                PrimitiveKind::CargoSmoke => Primitive::CargoSmoke(CargoSmokeArgs::default()),
                PrimitiveKind::AgentDone => Primitive::AgentDone(AgentDoneArgs {
                    reason: "test".into(),
                }),
            };
            assert_eq!(p.kind(), *kind);
        }
    }
}
