//! Native adapter — passthrough. A native runner driving the
//! daemon's `/v1/chat/completions` directly exposes ONLY the
//! canonical primitives to the model. The translation step is
//! mostly trivial: tool name is already canonical, args are parsed
//! against the typed primitive schema.

use serde_json::Value;

use crate::adapter::{AgentToolAdapter, TranslateOutcome};
use crate::descriptor::descriptors;
use crate::primitive::{
    AgentDoneArgs, CargoSmokeArgs, InspectIntent, Primitive, PrimitiveKind, WriteFileArgs,
};

/// Passthrough adapter — the canonical primitives ARE the agent's
/// tool set.
#[derive(Debug, Clone, Default)]
pub struct Adapter;

impl AgentToolAdapter for Adapter {
    fn id(&self) -> &'static str {
        "native"
    }

    fn tool_descriptors(&self) -> Vec<Value> {
        descriptors()
    }

    fn canonical_coverage(&self) -> Vec<PrimitiveKind> {
        PrimitiveKind::all().to_vec()
    }

    fn translate(&self, tool_name: &str, raw_args: &Value) -> TranslateOutcome {
        let Some(kind) = PrimitiveKind::from_id(tool_name) else {
            return TranslateOutcome::Unknown {
                tool_name: tool_name.to_string(),
            };
        };
        let parsed = match kind {
            PrimitiveKind::InspectWorkdir => raw_args
                .get("intent")
                .cloned()
                .and_then(|v| serde_json::from_value::<InspectIntent>(v).ok())
                .map(Primitive::InspectWorkdir),
            PrimitiveKind::WriteFile => {
                serde_json::from_value::<WriteFileArgs>(raw_args.clone())
                    .ok()
                    .map(Primitive::WriteFile)
            }
            PrimitiveKind::CargoBuild => Some(Primitive::CargoBuild),
            PrimitiveKind::CargoSmoke => {
                serde_json::from_value::<CargoSmokeArgs>(raw_args.clone())
                    .ok()
                    .or_else(|| Some(CargoSmokeArgs::default()))
                    .map(Primitive::CargoSmoke)
            }
            PrimitiveKind::AgentDone => {
                serde_json::from_value::<AgentDoneArgs>(raw_args.clone())
                    .ok()
                    .map(Primitive::AgentDone)
            }
        };
        match parsed {
            Some(canonical) => TranslateOutcome::Canonical {
                canonical,
                canonical_kind: kind,
            },
            None => TranslateOutcome::Unrecognized {
                tool_name: tool_name.to_string(),
                args_summary: cap_args_summary(raw_args),
                reason: format!("args failed to parse against {} schema", kind.id()),
            },
        }
    }
}

fn cap_args_summary(args: &Value) -> String {
    let s = args.to_string();
    if s.len() <= 200 {
        s
    } else {
        format!("{}…(+{} bytes)", &s[..200], s.len() - 200)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translate_write_file_canonical() {
        let a = Adapter;
        let r = a.translate(
            "write_file",
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
            other => panic!("expected canonical write_file, got {:?}", other),
        }
    }

    #[test]
    fn translate_inspect_file_intent() {
        let a = Adapter;
        let r = a.translate(
            "inspect_workdir",
            &json!({"intent": {"kind": "file", "path": "src/lib.rs"}}),
        );
        let canonical_kind = r.canonical_kind();
        assert_eq!(canonical_kind, Some(PrimitiveKind::InspectWorkdir));
    }

    #[test]
    fn translate_cargo_build_no_args() {
        let a = Adapter;
        let r = a.translate("cargo_build", &json!({}));
        assert_eq!(r.canonical_kind(), Some(PrimitiveKind::CargoBuild));
    }

    #[test]
    fn translate_unknown_tool() {
        let a = Adapter;
        let r = a.translate("hallucinated", &json!({}));
        match r {
            TranslateOutcome::Unknown { tool_name } => assert_eq!(tool_name, "hallucinated"),
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn translate_malformed_args_unrecognized() {
        let a = Adapter;
        // write_file without `content` field
        let r = a.translate("write_file", &json!({"path": "x"}));
        match r {
            TranslateOutcome::Unrecognized { tool_name, .. } => {
                assert_eq!(tool_name, "write_file");
            }
            other => panic!("expected Unrecognized, got {:?}", other),
        }
    }

    #[test]
    fn tool_descriptors_match_canonical_set() {
        let a = Adapter;
        let ds = a.tool_descriptors();
        assert_eq!(ds.len(), PrimitiveKind::all().len());
    }
}
