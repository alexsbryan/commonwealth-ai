// SPDX-License-Identifier: AGPL-3.0-or-later
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
    AgentDoneArgs, InspectIntent, Primitive, PrimitiveKind, SmokeArgs, WriteFileArgs,
};

/// Pi observer adapter. Per-problem build/verify commands are
/// installed via the builder so `BashIntent` classification matches
/// the actual cargo / go / pytest / vitest invocations the bench
/// configured for this problem, not hardcoded Rust strings.
#[derive(Debug, Clone, Default)]
pub struct Adapter {
    /// Prefix matched against pi's `bash { command }` to classify
    /// the build intent. Default empty = no match.
    build_cmd: Option<String>,
    /// Prefix matched against pi's `bash { command }` to classify
    /// the smoke intent.
    verify_cmd: Option<String>,
}

impl Adapter {
    /// Pi tool descriptors are pi-defined (read/write/bash/find/
    /// grep/ls). The bench currently passes them as a CLI
    /// allowlist; we expose the same list here so the bench can
    /// source it from the canonical crate rather than a hardcoded
    /// constant.
    pub fn pi_tool_allowlist() -> &'static [&'static str] {
        &["read", "write", "bash", "find", "grep", "ls"]
    }

    /// Bind the per-problem build/verify commands. The bench calls
    /// this when constructing a per-trial adapter from the loaded
    /// problem config. Both arguments are the same strings the
    /// bench uses for ExecCtx — so the pi adapter recognizes
    /// exactly what the native runner would have run.
    pub fn with_problem_commands(
        mut self,
        build_cmd: impl Into<String>,
        verify_cmd: impl Into<String>,
    ) -> Self {
        self.build_cmd = Some(build_cmd.into());
        self.verify_cmd = Some(verify_cmd.into());
        self
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
            "bash" => translate_bash(
                raw_args,
                self.build_cmd.as_deref(),
                self.verify_cmd.as_deref(),
            ),
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

/// Pi's `bash` is unconstrained. Match the command against the
/// per-problem build/verify command prefixes, or return
/// `Unrecognized` with the exact command for forensics.
fn translate_bash(
    args: &Value,
    build_prefix: Option<&str>,
    verify_prefix: Option<&str>,
) -> TranslateOutcome {
    let Some(cmd) = args.get("command").and_then(|v| v.as_str()) else {
        return TranslateOutcome::Unrecognized {
            tool_name: "bash".into(),
            args_summary: cap_args_summary(args),
            reason: "missing `command`".into(),
        };
    };
    match classify_bash(cmd, build_prefix, verify_prefix) {
        BashIntent::Build => TranslateOutcome::Canonical {
            canonical: Primitive::Build,
            canonical_kind: PrimitiveKind::Build,
        },
        BashIntent::Smoke { filter } => TranslateOutcome::Canonical {
            canonical: Primitive::Smoke(SmokeArgs { filter }),
            canonical_kind: PrimitiveKind::Smoke,
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
/// in the plan files for the convergence test ("does this map to a
/// canonical primitive?"). Pi's `bash` calls map to one of these;
/// per-problem prefixes are passed in at adapter construction so
/// Go / TS / Python problems get the same convergent surface.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BashIntent {
    Build,
    Smoke { filter: Option<String> },
    Unrecognized,
}

fn classify_bash(cmd: &str, build_prefix: Option<&str>, verify_prefix: Option<&str>) -> BashIntent {
    // Strip common shell wrappers + noise so prefix matching sees
    // the actual command head.
    let normalized = cmd
        .trim()
        .trim_start_matches("set -e ")
        .trim_start_matches("set -e\n")
        .trim();
    let head = normalized.split('|').next().unwrap_or(normalized).trim();
    let head = head.split("2>&1").next().unwrap_or(head).trim();

    // Defaults — if no per-problem commands are bound, fall back
    // to Rust prefixes (preserves PR 1 behavior for existing
    // Rust-only problems before the bench wiring lands).
    let build_match = build_prefix.unwrap_or("cargo build");
    let verify_match = verify_prefix.unwrap_or("cargo test --quiet --test integration");

    // Check verify_match FIRST when it's a strict prefix of
    // build_match (or vice versa) — longest match wins.
    let (first, first_intent, second, second_intent): (&str, BashIntent, &str, BashIntent) =
        if verify_match.len() >= build_match.len() {
            (
                verify_match,
                BashIntent::Smoke { filter: None },
                build_match,
                BashIntent::Build,
            )
        } else {
            (
                build_match,
                BashIntent::Build,
                verify_match,
                BashIntent::Smoke { filter: None },
            )
        };

    if matches_command_prefix(head, first) {
        return refine_intent(head, first, first_intent);
    }
    if matches_command_prefix(head, second) {
        return refine_intent(head, second, second_intent);
    }
    BashIntent::Unrecognized
}

/// True when `head` begins with `prefix` after token boundary —
/// i.e. `head == prefix` OR `head` starts with `prefix` followed by
/// whitespace. Prevents false matches like "cargo build-script"
/// for a "cargo build" prefix.
fn matches_command_prefix(head: &str, prefix: &str) -> bool {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return false;
    }
    let head_norm: String = head.split_whitespace().collect::<Vec<_>>().join(" ");
    let prefix_norm: String = prefix.split_whitespace().collect::<Vec<_>>().join(" ");
    if head_norm == prefix_norm {
        return true;
    }
    head_norm.starts_with(&format!("{prefix_norm} "))
}

/// For the Smoke intent, extract an optional filter token from
/// the head if the bound verify_cmd ends in `--test integration`
/// or similar (libtest convention). Falls back to None for other
/// runners.
fn refine_intent(head: &str, prefix: &str, base: BashIntent) -> BashIntent {
    match base {
        BashIntent::Smoke { .. } => {
            let head_norm: String = head.split_whitespace().collect::<Vec<_>>().join(" ");
            let prefix_norm: String = prefix.split_whitespace().collect::<Vec<_>>().join(" ");
            let tail = head_norm.strip_prefix(&prefix_norm).unwrap_or("").trim();
            let filter = tail
                .split_whitespace()
                .next()
                .filter(|tok| !tok.starts_with("--"))
                .map(|s| s.to_string());
            BashIntent::Smoke { filter }
        }
        other => other,
    }
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
        let a = Adapter::default();
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
        let a = Adapter::default();
        let r = a.translate("ls", &json!({"path": "src"}));
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn find_to_inspect_find_by_name() {
        let a = Adapter::default();
        let r = a.translate("find", &json!({"root": ".", "pattern": "*.rs"}));
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn grep_to_inspect_grep_contents() {
        let a = Adapter::default();
        let r = a.translate("grep", &json!({"root": "src", "pattern": "todo"}));
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn write_to_write_file() {
        let a = Adapter::default();
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

    fn rust_defaults() -> (Option<&'static str>, Option<&'static str>) {
        (
            Some("cargo build"),
            Some("cargo test --quiet --test integration"),
        )
    }

    #[test]
    fn bash_cargo_build_classifies_as_build() {
        let (b, v) = rust_defaults();
        assert_eq!(classify_bash("cargo build", b, v), BashIntent::Build);
        assert_eq!(classify_bash("cargo build 2>&1", b, v), BashIntent::Build);
        assert_eq!(
            classify_bash("cargo build --release", b, v),
            BashIntent::Build
        );
    }

    #[test]
    fn bash_cargo_test_integration_classifies_as_smoke() {
        let (b, v) = rust_defaults();
        match classify_bash("cargo test --quiet --test integration", b, v) {
            BashIntent::Smoke { filter: None } => {}
            other => panic!("got {:?}", other),
        }
        match classify_bash("cargo test --quiet --test integration my_test_name", b, v) {
            BashIntent::Smoke { filter: Some(f) } => assert_eq!(f, "my_test_name"),
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn bash_workspace_test_is_unrecognized() {
        let (b, v) = rust_defaults();
        assert_eq!(
            classify_bash("cargo test --workspace", b, v),
            BashIntent::Unrecognized
        );
    }

    #[test]
    fn bash_arbitrary_shell_is_unrecognized() {
        let (b, v) = rust_defaults();
        assert_eq!(classify_bash("ls -la", b, v), BashIntent::Unrecognized);
        assert_eq!(classify_bash("rm -rf /", b, v), BashIntent::Unrecognized);
        assert_eq!(
            classify_bash("python -c 'print(1)'", b, v),
            BashIntent::Unrecognized
        );
    }

    #[test]
    fn bash_per_language_go_build_classifies_as_build() {
        // Multi-language: pi adapter bound to Go problem commands
        // recognizes `go build ./...` as canonical Build.
        let r = classify_bash(
            "go build ./... 2>&1",
            Some("go build ./..."),
            Some("go test ./..."),
        );
        assert_eq!(r, BashIntent::Build);
    }

    #[test]
    fn bash_per_language_pytest_classifies_as_smoke() {
        let r = classify_bash(
            "pytest -v tests/",
            Some(""), // python has no build step
            Some("pytest -v tests/"),
        );
        match r {
            BashIntent::Smoke { .. } => {}
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn bash_translate_emits_unrecognized_outcome_for_arbitrary_command() {
        let a = Adapter::default();
        let r = a.translate("bash", &json!({"command": "echo hi"}));
        match r {
            TranslateOutcome::Unrecognized {
                tool_name, reason, ..
            } => {
                assert_eq!(tool_name, "bash");
                assert!(reason.contains("echo hi"));
            }
            other => panic!("expected Unrecognized, got {:?}", other),
        }
    }

    #[test]
    fn done_to_agent_done() {
        let a = Adapter::default();
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
        let a = Adapter::default();
        let r = a.translate("not_a_tool", &json!({}));
        match r {
            TranslateOutcome::Unknown { tool_name } => assert_eq!(tool_name, "not_a_tool"),
            other => panic!("got {:?}", other),
        }
    }
}
