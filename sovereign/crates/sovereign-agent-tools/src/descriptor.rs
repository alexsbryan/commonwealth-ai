// SPDX-License-Identifier: AGPL-3.0-or-later
//! JSON-Schema descriptors for each primitive. This is the shape
//! the model sees in the OpenAI chat completion `tools` array.
//!
//! Schemas are hand-written rather than derived (the derive crates
//! add a non-trivial dep + introduce subtle deviations from what
//! the model has been trained on). The `descriptors()` fn is
//! exhaustive over `PrimitiveKind::all()` — adding a variant
//! without a descriptor fails the equivalence test.

use serde_json::{json, Value};

use crate::primitive::PrimitiveKind;

/// Build the tool descriptor for a single primitive — OpenAI-shape
/// `{"type": "function", "function": {name, description, parameters}}`.
pub fn descriptor_for(kind: PrimitiveKind) -> Value {
    let (description, parameters) = match kind {
        PrimitiveKind::InspectWorkdir => (
            "Read state from the workdir. Polymorphic by `intent`: read a file, list a directory, find files by name pattern, or grep file contents. Use this BEFORE writing — looking up the current state of a file you intend to modify is cheaper than guessing.",
            json!({
                "type": "object",
                "properties": {
                    "intent": {
                        "oneOf": [
                            {"type": "object", "properties": {"kind": {"const": "file"}, "path": {"type": "string"}}, "required": ["kind", "path"]},
                            {"type": "object", "properties": {"kind": {"const": "dir"}, "path": {"type": "string"}}, "required": ["kind", "path"]},
                            {"type": "object", "properties": {"kind": {"const": "find_by_name"}, "root": {"type": "string"}, "pattern": {"type": "string"}}, "required": ["kind", "root", "pattern"]},
                            {"type": "object", "properties": {"kind": {"const": "grep_contents"}, "root": {"type": "string"}, "pattern": {"type": "string"}}, "required": ["kind", "root", "pattern"]}
                        ]
                    }
                },
                "required": ["intent"]
            }),
        ),
        PrimitiveKind::WriteFile => (
            "Atomically replace the contents of a file in the workdir with `content`. Creates parent directories if needed. Whole-file replacement — for targeted edits to an existing file prefer `patch_file`, which has less JSON-escape pressure on `content` and is bounded in scope. Does NOT run any build step; call `build` next to verify the change compiled.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workdir-relative path."},
                    "content": {"type": "string", "description": "Complete file body."}
                },
                "required": ["path", "content"]
            }),
        ),
        PrimitiveKind::PatchFile => (
            "Replace a contiguous range of lines in an existing file with `new_content`. Lines are 1-indexed and inclusive: `start_line=5, end_line=7` replaces lines 5, 6, 7. `new_content` may be multi-line (split on `\\n`) or empty (deletes the range). Use this for targeted edits — it's smaller than `write_file` (less JSON-escape pressure) and the resulting full content is syntax-checked at the write boundary. For initial file creation or full rewrites, use `write_file` instead.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workdir-relative path. The file must already exist."},
                    "start_line": {"type": "integer", "description": "1-indexed inclusive start line."},
                    "end_line": {"type": "integer", "description": "1-indexed inclusive end line (>= start_line)."},
                    "new_content": {"type": "string", "description": "Replacement content. Multi-line OK. Empty deletes the range."}
                },
                "required": ["path", "start_line", "end_line", "new_content"]
            }),
        ),
        PrimitiveKind::ReplaceFunction => (
            "Replace the entire definition of a named function or class with `new_body`. Smaller output surface than `patch_file` (no line ranges to count — just emit the function body). PREFERRED for single-function bug fixes when you know the function name and want to rewrite its body cleanly. The post-replace full file is syntax-checked at the write boundary. `name` is a plain identifier (e.g. `tokenize`, not `Parser.tokenize`). `new_body` must start with the `def`/`class` line at the correct indent and include the full body.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workdir-relative path. File must exist."},
                    "name": {"type": "string", "description": "Function or class name to replace."},
                    "new_body": {"type": "string", "description": "Full new definition: `def NAME(...):` or `class NAME(...):` + body. Match the original's indentation."}
                },
                "required": ["path", "name", "new_body"]
            }),
        ),
        PrimitiveKind::Build => (
            "Run the bench-bound build command in the workdir (cargo for Rust, go build for Go, tsc for TypeScript, no-op for Python). Reports pass/fail plus the command's output tail. Use after `write_file` to check your change typechecks before moving on.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        PrimitiveKind::Smoke => (
            "Run the bench-bound integration test command (cargo test / go test / vitest / pytest) and report parsed pass/fail counts plus failing-test names. Optional `filter` is appended as a positional argument; the per-language runner interprets it as a test-name substring or expression. Use AFTER `build` reports ok.",
            json!({
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "description": "Optional test-name filter."}
                }
            }),
        ),
        PrimitiveKind::AgentDone => (
            "Signal that you have finished. Provide a `reason` summarizing the final state (e.g. 'all integration tests pass'). This terminates the run; the grader's held-out test suite then runs against your workdir.",
            json!({
                "type": "object",
                "properties": {
                    "reason": {"type": "string"}
                },
                "required": ["reason"]
            }),
        ),
        PrimitiveKind::AgentPlan => (
            "Emit the plan. `plan` is a 3-6 sentence high-level approach (data structures, algorithm, files to write). For problems with MULTIPLE distinct edits (bug-fix tasks, multi-feature implementations), ALSO fill `pseudocode` — a numbered list of every concrete change the Implementer will need to make, one entry per change, each naming the target (function/file/line-range) AND the approach. Example: \"1. tokenize (lines 75-77): add two-char lex for <=, >=, ==, != BEFORE the single-char fall-through\". The pseudocode list stays pinned in every Implementer turn so each patch can be informed by the full plan, not just the most recent diagnosis. Optionally list `files_to_create` for net-new files. Available only to the Planner role; calling this hands off to the Implementer.",
            json!({
                "type": "object",
                "properties": {
                    "plan": {"type": "string"},
                    "files_to_create": {"type": "array", "items": {"type": "string"}},
                    "pseudocode": {"type": "array", "items": {"type": "string"}, "description": "Numbered concrete change list; one entry per distinct edit."}
                },
                "required": ["plan"]
            }),
        ),
        PrimitiveKind::HandoffToEvaluator => (
            "Signal that your write is complete and verification should run next. One-line `what_you_changed` summary. Available to the Implementer role; calling this transfers control to the Evaluator.",
            json!({
                "type": "object",
                "properties": {
                    "what_you_changed": {"type": "string"}
                },
                "required": ["what_you_changed"]
            }),
        ),
        PrimitiveKind::HandoffToImplementer => (
            "Send control back to the Implementer with a diagnosis. Use when build or smoke revealed an issue. One-paragraph `diagnosis`. Available to the Evaluator role.",
            json!({
                "type": "object",
                "properties": {
                    "diagnosis": {"type": "string"}
                },
                "required": ["diagnosis"]
            }),
        ),
    };
    json!({
        "type": "function",
        "function": {
            "name": kind.id(),
            "description": description,
            "parameters": parameters,
        }
    })
}

/// Build all canonical primitive descriptors. This is the array a
/// native runner passes to the daemon's chat completion request as
/// `tools`.
pub fn descriptors() -> Vec<Value> {
    PrimitiveKind::all()
        .iter()
        .copied()
        .map(descriptor_for)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_are_exhaustive_over_primitive_kind() {
        let ds = descriptors();
        assert_eq!(ds.len(), PrimitiveKind::all().len());
        for (kind, descr) in PrimitiveKind::all().iter().zip(ds.iter()) {
            let name = descr
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str());
            assert_eq!(name, Some(kind.id()));
        }
    }

    #[test]
    fn every_descriptor_has_required_oai_shape() {
        for d in descriptors() {
            assert_eq!(d.get("type").and_then(|v| v.as_str()), Some("function"));
            assert!(d.get("function").is_some());
            let f = d.get("function").unwrap();
            assert!(f.get("name").is_some());
            assert!(f.get("description").is_some());
            assert!(f.get("parameters").is_some());
        }
    }
}
