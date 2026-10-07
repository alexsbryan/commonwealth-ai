// SPDX-License-Identifier: AGPL-3.0-or-later
//! opencode's built-in tools, mapped onto the canonical primitives.
//!
//! opencode's tools mean what pi's mean under other argument names (`read`
//! and `write` take `filePath`, `list` is pi's `ls`, `glob` is pi's `find`),
//! so this adapter renames the arguments and hands the call to the pi
//! adapter: one decider for what a write of a path or a `bash` of the
//! problem's verify command is, whichever agent made it.
//!
//! Two honest gaps. opencode's `edit` replaces an exact string, and the
//! nearest primitive (`PatchFile`) replaces a line range, so an edit is
//! `Unrecognized` with that reason rather than forced into the wrong
//! shape. And opencode has no `done` tool (it finishes by answering
//! without a tool call), so [`AgentToolAdapter::canonical_coverage`] omits
//! `AgentDone`.

use serde_json::Value;

use crate::adapter::{pi, AgentToolAdapter, TranslateOutcome};
use crate::primitive::PrimitiveKind;

#[derive(Debug, Clone, Default)]
pub struct Adapter {
    pi: pi::Adapter,
}

impl Adapter {
    /// The problem's build and verify commands, which classify a `bash`.
    pub fn with_problem_commands(
        mut self,
        build_cmd: impl Into<String>,
        verify_cmd: impl Into<String>,
    ) -> Self {
        self.pi = self.pi.with_problem_commands(build_cmd, verify_cmd);
        self
    }
}

impl AgentToolAdapter for Adapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    /// opencode defines its own tools; nothing is presented by us.
    fn tool_descriptors(&self) -> Vec<Value> {
        Vec::new()
    }

    fn canonical_coverage(&self) -> Vec<PrimitiveKind> {
        PrimitiveKind::all()
            .iter()
            .copied()
            .filter(|k| *k != PrimitiveKind::AgentDone)
            .collect()
    }

    fn translate(&self, tool_name: &str, raw_args: &Value) -> TranslateOutcome {
        match tool_name {
            "read" => self.pi.translate("read", &file_path_as_path(raw_args)),
            "write" => self.pi.translate("write", &file_path_as_path(raw_args)),
            "list" => self.pi.translate("ls", raw_args),
            "glob" => self.pi.translate("find", raw_args),
            "grep" => self.pi.translate("grep", raw_args),
            "bash" => self.pi.translate("bash", raw_args),
            "edit" | "multiedit" | "patch" => TranslateOutcome::Unrecognized {
                tool_name: tool_name.to_string(),
                args_summary: raw_args
                    .get("filePath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                reason: "exact-string edit has no canonical primitive \
                         (PatchFile replaces a line range)"
                    .into(),
            },
            other => TranslateOutcome::Unknown {
                tool_name: other.to_string(),
            },
        }
    }
}

/// opencode names the file `filePath`; pi's translators read `path`.
fn file_path_as_path(args: &Value) -> Value {
    let mut out = args.clone();
    if let (Some(obj), Some(p)) = (out.as_object_mut(), args.get("filePath").cloned()) {
        obj.entry("path").or_insert(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn write_of_a_file_path_is_a_canonical_write() {
        let a = Adapter::default();
        let out = a.translate(
            "write",
            &json!({"filePath": "/w/fib.py", "content": "x = 1\n"}),
        );
        assert_eq!(out.canonical_kind(), Some(PrimitiveKind::WriteFile));
    }

    #[test]
    fn bash_is_classified_by_the_problem_commands() {
        let a = Adapter::default().with_problem_commands("python3 -m py_compile", "pytest -q");
        let verify = a.translate("bash", &json!({"command": "pytest -q tests"}));
        assert_eq!(verify.canonical_kind(), Some(PrimitiveKind::Smoke));
        let other = a.translate("bash", &json!({"command": "ls -la"}));
        assert!(matches!(other, TranslateOutcome::Unrecognized { .. }));
    }

    #[test]
    fn edit_is_named_unrecognized_not_forced_into_patch_file() {
        let a = Adapter::default();
        let out = a.translate(
            "edit",
            &json!({"filePath": "/w/a.py", "oldString": "a", "newString": "b"}),
        );
        match out {
            TranslateOutcome::Unrecognized {
                tool_name, reason, ..
            } => {
                assert_eq!(tool_name, "edit");
                assert!(reason.contains("line range"));
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn coverage_is_pis_less_agent_done() {
        let pi: std::collections::HashSet<_> = pi::Adapter::default()
            .canonical_coverage()
            .into_iter()
            .collect();
        let oc: std::collections::HashSet<_> = Adapter::default()
            .canonical_coverage()
            .into_iter()
            .collect();
        let gap: Vec<_> = pi.difference(&oc).copied().collect();
        assert_eq!(gap, vec![PrimitiveKind::AgentDone]);
        assert!(oc.is_subset(&pi));
    }
}
