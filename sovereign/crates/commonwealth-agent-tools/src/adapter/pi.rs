//! Pi adapter — observer-only.
//!
//! Pi keeps its own tool registry (`read`, `write`, `bash`, `find`,
//! `grep`, `ls`) and executes tools inside its own subprocess. We
//! cannot change that. What we CAN do is record pi's behavior in
//! the canonical shape so cross-agent comparison is well-defined.
//!
//! Translation rules:
//!
//! | pi tool | canonical primitive |
//! |---|---|
//! | `read(path)` | `inspect_workdir` with `File` intent |
//! | `ls(path)` | `inspect_workdir` with `Dir` intent |
//! | `find(root, pattern)` | `inspect_workdir` with `FindByName` intent |
//! | `grep(root, pattern)` | `inspect_workdir` with `GrepContents` intent |
//! | `write(path, content)` | `write_file` |
//! | `bash` | inspected, see below |
//! | `done(reason)` | `agent_done` |
//!
//! `bash` is a closed-enum `BashIntent`:
//! - `cargo build [...]` → `cargo_build`
//! - `cargo test --test integration [filter?]` → `cargo_smoke`
//! - anything else → `TranslateOutcome::Unrecognized` (records the
//!   actual command verbatim for forensics; does NOT silently
//!   drop the call)
//!
//! The canonical_coverage MUST equal native's. Per the test in
//! adapter/mod.rs, pi must cover every primitive the native set
//! has — if a future PR adds a primitive to native, this adapter
//! must learn to recognize the pi-side variant.

use serde_json::Value;

use crate::adapter::{AgentToolAdapter, TranslateOutcome};
use crate::primitive::{
    AgentDoneArgs, CargoSmokeArgs, InspectIntent, Primitive, PrimitiveKind, WriteFileArgs,
};

/// Pi observer adapter.
#[derive(Debug, Clone, Default)]
pub struct Adapter;

impl Adapter {
    /// Pi tool descriptors are pi-defined (read/write/bash/find/
    /// grep/ls). The bench currently passes them as a CLI
    /// allowlist; we expose the same list here so the bench can
    /// source it from the canonical crate rather than a hardcoded
    /// constant.
    pub fn pi_tool_allowlist() -> &'static [&'static str] {
        &["read", "write", "bash", "find", "grep", "ls"]
    }
}

impl AgentToolAdapter for Adapter {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn tool_descriptors(&self) -> Vec<Value> {
        // Pi defines its own tool schemas inside the pi-coding-
        // agent binary. The adapter doesn't reshape them — that
        // would be a fork. The bench passes the allowlist to pi
        // via `--tools`; pi assembles the schema itself. We
        // return an empty Vec to signal "no descriptors of our
        // own" — the runner consults `pi_tool_allowlist()` for
        // the CLI flag instead.
        Vec::new()
    }

    fn canonical_coverage(&self) -> Vec<PrimitiveKind> {
        // Every canonical primitive has a pi-side mapping. The
        // equivalence test (adapter/mod.rs) pins this against
        // native — drift fails the test.
        PrimitiveKind::all().to_vec()
    }

    fn translate(&self, tool_name: &str, raw_args: &Value) -> TranslateOutcome {
        match tool_name {
            "read" => translate_read(raw_args),
            "ls" => translate_ls(raw_args),
            "find" => translate_find(raw_args),
            "grep" => translate_grep(raw_args),
            "write" => translate_write(raw_args),
            "bash" => translate_bash(raw_args),
            "done" => translate_done(raw_args),
            other => TranslateOutcome::Unknown {
                tool_name: other.to_string(),
            },
        }
    }
}

fn translate_read(args: &Value) -> TranslateOutcome {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return TranslateOutcome::Unrecognized {
            tool_name: "read".into(),
            args_summary: cap_args_summary(args),
            reason: "missing `path`".into(),
        };
    };
    TranslateOutcome::Canonical {
        canonical: Primitive::InspectWorkdir(InspectIntent::File {
            path: path.to_string(),
        }),
        canonical_kind: PrimitiveKind::InspectWorkdir,
    }
}

fn translate_ls(args: &Value) -> TranslateOutcome {
    // Pi's `ls` may pass `path` or default to the workdir root.
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    TranslateOutcome::Canonical {
        canonical: Primitive::InspectWorkdir(InspectIntent::Dir { path }),
        canonical_kind: PrimitiveKind::InspectWorkdir,
    }
}

fn translate_find(args: &Value) -> TranslateOutcome {
    let root = args
        .get("root")
        .or_else(|| args.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let Some(pattern) = args
        .get("pattern")
        .or_else(|| args.get("name"))
        .and_then(|v| v.as_str())
    else {
        return TranslateOutcome::Unrecognized {
            tool_name: "find".into(),
            args_summary: cap_args_summary(args),
            reason: "missing `pattern` / `name`".into(),
        };
    };
    TranslateOutcome::Canonical {
        canonical: Primitive::InspectWorkdir(InspectIntent::FindByName {
            root,
            pattern: pattern.to_string(),
        }),
        canonical_kind: PrimitiveKind::InspectWorkdir,
    }
}

fn translate_grep(args: &Value) -> TranslateOutcome {
    let root = args
        .get("root")
        .or_else(|| args.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or(".")
        .to_string();
    let Some(pattern) = args.get("pattern").and_then(|v| v.as_str()) else {
        return TranslateOutcome::Unrecognized {
            tool_name: "grep".into(),
            args_summary: cap_args_summary(args),
            reason: "missing `pattern`".into(),
        };
    };
    TranslateOutcome::Canonical {
        canonical: Primitive::InspectWorkdir(InspectIntent::GrepContents {
            root,
            pattern: pattern.to_string(),
        }),
        canonical_kind: PrimitiveKind::InspectWorkdir,
    }
}

fn translate_write(args: &Value) -> TranslateOutcome {
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return TranslateOutcome::Unrecognized {
            tool_name: "write".into(),
            args_summary: cap_args_summary(args),
            reason: "missing `path`".into(),
        };
    };
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    TranslateOutcome::Canonical {
        canonical: Primitive::WriteFile(WriteFileArgs {
            path: path.to_string(),
            content,
        }),
        canonical_kind: PrimitiveKind::WriteFile,
    }
}

fn translate_done(args: &Value) -> TranslateOutcome {
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    TranslateOutcome::Canonical {
        canonical: Primitive::AgentDone(AgentDoneArgs { reason }),
        canonical_kind: PrimitiveKind::AgentDone,
    }
}

/// Pi's `bash` is unconstrained. Inspect the command and map to
/// the canonical set, or return `Unrecognized` with the exact
/// command for forensics.
fn translate_bash(args: &Value) -> TranslateOutcome {
    let Some(cmd) = args.get("command").and_then(|v| v.as_str()) else {
        return TranslateOutcome::Unrecognized {
            tool_name: "bash".into(),
            args_summary: cap_args_summary(args),
            reason: "missing `command`".into(),
        };
    };
    match classify_bash(cmd) {
        BashIntent::Build => TranslateOutcome::Canonical {
            canonical: Primitive::CargoBuild,
            canonical_kind: PrimitiveKind::CargoBuild,
        },
        BashIntent::Smoke { filter } => TranslateOutcome::Canonical {
            canonical: Primitive::CargoSmoke(CargoSmokeArgs { filter }),
            canonical_kind: PrimitiveKind::CargoSmoke,
        },
        BashIntent::Unrecognized => TranslateOutcome::Unrecognized {
            tool_name: "bash".into(),
            args_summary: cap_args_summary(args),
            reason: format!("bash command not in canonical set: `{}`", cap_str(cmd, 200)),
        },
    }
}

/// Closed enum of bash commands the canonical layer recognizes.
/// Adding a variant is intentionally heavy — see the methodology
/// in `~/.claude/plans/autonomous-loop-tick-tingly-clock.md` for
/// the convergence test ("does this map to a canonical
/// primitive?").
#[derive(Debug, Clone, PartialEq, Eq)]
enum BashIntent {
    Build,
    Smoke { filter: Option<String> },
    Unrecognized,
}

fn classify_bash(cmd: &str) -> BashIntent {
    // Strip common shell prefixes pi tends to emit ("cargo build
    // 2>&1", "cd workdir && cargo test ..."). We don't try to be
    // a real shell — pi's actual cwd is the workdir already; the
    // adapter just inspects the cargo invocation.
    let normalized = cmd
        .trim()
        .trim_start_matches("set -e ")
        .trim_start_matches("set -e\n")
        .trim();

    // Common suffix noise like " 2>&1" or " | head -50" — drop the
    // pipe tail so we look at the head of the pipeline only.
    let head = normalized.split('|').next().unwrap_or(normalized).trim();
    let head = head.split("2>&1").next().unwrap_or(head).trim();

    // `cargo test --test integration [filter]` — order matters
    // (check the more specific before `cargo build`).
    if let Some(rest) = head.strip_prefix("cargo test ") {
        // The integration suite is the canonical bench surface;
        // anything else (workspace test, etc.) is not the same
        // class of action and gets recorded as Unrecognized so
        // operators can see what the agent ran.
        if rest.contains("--test integration") {
            // Optional positional filter after the flag block.
            let filter = rest
                .split("--test integration")
                .nth(1)
                .and_then(|tail| tail.trim().split_whitespace().next())
                .filter(|tok| !tok.starts_with("--"))
                .map(|s| s.to_string());
            return BashIntent::Smoke { filter };
        }
        return BashIntent::Unrecognized;
    }

    if head.starts_with("cargo build") {
        return BashIntent::Build;
    }

    BashIntent::Unrecognized
}

fn cap_args_summary(args: &Value) -> String {
    cap_str(&args.to_string(), 200)
}

fn cap_str(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}…(+{} bytes)", &s[..limit], s.len() - limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_to_inspect_file() {
        let a = Adapter;
        let r = a.translate("read", &json!({"path": "src/lib.rs"}));
        match r {
            TranslateOutcome::Canonical {
                canonical: Primitive::InspectWorkdir(InspectIntent::File { path }),
                canonical_kind: PrimitiveKind::InspectWorkdir,
            } => assert_eq!(path, "src/lib.rs"),
            other => panic!("expected inspect File, got {:?}", other),
        }
    }

    #[test]
    fn ls_to_inspect_dir() {
        let a = Adapter;
        let r = a.translate("ls", &json!({"path": "src"}));
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn find_to_inspect_find_by_name() {
        let a = Adapter;
        let r = a.translate(
            "find",
            &json!({"root": ".", "pattern": "*.rs"}),
        );
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn grep_to_inspect_grep_contents() {
        let a = Adapter;
        let r = a.translate("grep", &json!({"root": "src", "pattern": "todo"}));
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn write_to_write_file() {
        let a = Adapter;
        let r = a.translate(
            "write",
            &json!({"path": "src/lib.rs", "content": "pub fn x() {}"}),
        );
        match r {
            TranslateOutcome::Canonical {
                canonical: Primitive::WriteFile(args),
                canonical_kind: PrimitiveKind::WriteFile,
            } => {
                assert_eq!(args.path, "src/lib.rs");
                assert_eq!(args.content, "pub fn x() {}");
            }
            other => panic!("expected write_file, got {:?}", other),
        }
    }

    #[test]
    fn bash_cargo_build_classifies_as_build() {
        assert_eq!(classify_bash("cargo build"), BashIntent::Build);
        assert_eq!(classify_bash("cargo build 2>&1"), BashIntent::Build);
        assert_eq!(
            classify_bash("cargo build --release"),
            BashIntent::Build
        );
    }

    #[test]
    fn bash_cargo_test_integration_classifies_as_smoke() {
        match classify_bash("cargo test --quiet --test integration") {
            BashIntent::Smoke { filter: None } => {}
            other => panic!("got {:?}", other),
        }
        match classify_bash("cargo test --quiet --test integration my_test_name") {
            BashIntent::Smoke { filter: Some(f) } => assert_eq!(f, "my_test_name"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn bash_workspace_test_is_unrecognized() {
        // Workspace `cargo test` (no --test integration) is NOT
        // the canonical smoke shape — it would run the wrong
        // suite. Adapter records as Unrecognized so the model's
        // choice is honestly captured.
        assert_eq!(
            classify_bash("cargo test --workspace"),
            BashIntent::Unrecognized
        );
    }

    #[test]
    fn bash_arbitrary_shell_is_unrecognized() {
        assert_eq!(classify_bash("ls -la"), BashIntent::Unrecognized);
        assert_eq!(classify_bash("rm -rf /"), BashIntent::Unrecognized);
        assert_eq!(classify_bash("python -c 'print(1)'"), BashIntent::Unrecognized);
    }

    #[test]
    fn bash_translate_emits_unrecognized_outcome_for_arbitrary_command() {
        let a = Adapter;
        let r = a.translate("bash", &json!({"command": "echo hi"}));
        match r {
            TranslateOutcome::Unrecognized {
                tool_name,
                reason,
                ..
            } => {
                assert_eq!(tool_name, "bash");
                assert!(reason.contains("echo hi"));
            }
            other => panic!("expected Unrecognized, got {:?}", other),
        }
    }

    #[test]
    fn done_to_agent_done() {
        let a = Adapter;
        let r = a.translate("done", &json!({"reason": "all tests pass"}));
        match r {
            TranslateOutcome::Canonical {
                canonical: Primitive::AgentDone(args),
                canonical_kind: PrimitiveKind::AgentDone,
            } => assert_eq!(args.reason, "all tests pass"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn unknown_tool_name() {
        let a = Adapter;
        let r = a.translate("not_a_tool", &json!({}));
        match r {
            TranslateOutcome::Unknown { tool_name } => assert_eq!(tool_name, "not_a_tool"),
            other => panic!("got {:?}", other),
        }
    }
}
