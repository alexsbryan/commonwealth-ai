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
            "Atomically replace the contents of a file in the workdir with `content`. Creates parent directories if needed. Whole-file replacement — there is no diff/edit form (the exact-match brittleness of edit-tools has bitten this bench before, see HANDOFF.md). Does NOT run cargo; call `cargo_build` next to verify your change compiled.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Workdir-relative path."},
                    "content": {"type": "string", "description": "Complete file body."}
                },
                "required": ["path", "content"]
            }),
        ),
        PrimitiveKind::CargoBuild => (
            "Run `cargo build` in the workdir and report pass/fail with compile errors. Use this after `write_file` to check your change typechecks before moving on.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        PrimitiveKind::CargoSmoke => (
            "Run the integration test suite (`cargo test --test integration`) and report parsed pass/fail counts plus failing-test names. Optional `filter` runs only tests whose names contain the substring. Use this AFTER `cargo_build` reports ok to verify behavioral correctness.",
            json!({
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "description": "Optional test-name substring filter."}
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
    PrimitiveKind::all().iter().copied().map(descriptor_for).collect()
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
