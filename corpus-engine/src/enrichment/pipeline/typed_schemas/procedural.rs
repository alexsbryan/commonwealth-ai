//! Procedural discourse-mode Phase 1 — system prompt, schema, parser.
//!
//! Fires when the Phase 0 classifier surfaces `DiscourseMode::Procedural`
//! above the routing threshold. Atoms: tasks, decisions, artifacts,
//! dependencies, blockers, status_signals.

use crate::enrichment::pipeline::atlas::{
    ArtifactSketch, BlockerSketch, DecisionSketch, DependencySketch, ProceduralExtension,
    StatusSignalSketch, TaskSketch, TypeExtension,
};
use crate::enrichment::pipeline::types::strip_reasoning_tags;
use crate::error::{Error, Result};

pub const PHASE1_PROCEDURAL_SYSTEM: &str =
    include_str!("procedural_phase1_system.md");

pub fn phase1_procedural_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "owner": { "type": "string" },
                        "due_at": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "alternatives": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "artifacts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": { "type": "string", "minLength": 1 },
                        "description": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "dependencies": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["from", "to"],
                    "properties": {
                        "from": { "type": "string", "minLength": 1 },
                        "to": { "type": "string", "minLength": 1 },
                        "kind": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "blockers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string", "minLength": 1 },
                        "blocks": { "type": "string" },
                        "anchor": { "type": "string" }
                    }
                }
            },
            "status_signals": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["state", "content"],
                    "properties": {
                        "state": { "type": "string", "minLength": 1 },
                        "content": { "type": "string", "minLength": 1 },
                        "anchor": { "type": "string" }
                    }
                }
            }
        }
    })
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn required_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn parse_phase1_procedural(response: &str) -> Result<ProceduralExtension> {
    let stripped = strip_reasoning_tags(response);
    let cleaned: String = crate::enrichment::pipeline::types::extract_json_block(&stripped)
        .map(|s| s.to_string())
        .unwrap_or_else(|| stripped.clone());
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "procedural typed-extension response is not valid JSON: {e}; \
             body head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let tasks: Vec<TaskSketch> = v
        .get("tasks")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(TaskSketch {
                        content: required_str(e, "content")?,
                        owner: str_field(e, "owner"),
                        due_at: str_field(e, "due_at"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let decisions: Vec<DecisionSketch> = v
        .get("decisions")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let content = required_str(e, "content")?;
                    let alternatives = e
                        .get("alternatives")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| {
                                    p.as_str().map(str::trim).filter(|s| !s.is_empty())
                                })
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    Some(DecisionSketch {
                        content,
                        alternatives,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let artifacts: Vec<ArtifactSketch> = v
        .get("artifacts")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(ArtifactSketch {
                        name: required_str(e, "name")?,
                        description: str_field(e, "description"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let dependencies: Vec<DependencySketch> = v
        .get("dependencies")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(DependencySketch {
                        from: required_str(e, "from")?,
                        to: required_str(e, "to")?,
                        kind: str_field(e, "kind"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let blockers: Vec<BlockerSketch> = v
        .get("blockers")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(BlockerSketch {
                        content: required_str(e, "content")?,
                        blocks: str_field(e, "blocks"),
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let status_signals: Vec<StatusSignalSketch> = v
        .get("status_signals")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    Some(StatusSignalSketch {
                        state: required_str(e, "state")?,
                        content: required_str(e, "content")?,
                        anchor: str_field(e, "anchor"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ProceduralExtension {
        tasks,
        decisions,
        artifacts,
        dependencies,
        blockers,
        status_signals,
    })
}

pub fn parse_phase1_procedural_extension(response: &str) -> Result<TypeExtension> {
    Ok(TypeExtension::Procedural(parse_phase1_procedural(response)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_decision_pair() {
        let json = r#"{
            "tasks":[{"content":"Ship gating change.","owner":"alex","due_at":"by Thursday"}],
            "decisions":[{"content":"Adopt Postgres.","alternatives":["MongoDB","SQLite"]}]
        }"#;
        let e = parse_phase1_procedural(json).expect("parses");
        assert_eq!(e.tasks.len(), 1);
        assert_eq!(e.tasks[0].owner, "alex");
        assert_eq!(e.decisions[0].alternatives.len(), 2);
    }

    #[test]
    fn empty_object_yields_empty_extension() {
        let e = parse_phase1_procedural("{}").expect("parses");
        assert_eq!(e.atom_count(), 0);
    }

    #[test]
    fn dependency_requires_both_endpoints() {
        let json = r#"{"dependencies":[{"from":"A","to":""},{"from":"B","to":"C"}]}"#;
        let e = parse_phase1_procedural(json).expect("parses");
        assert_eq!(e.dependencies.len(), 1);
        assert_eq!(e.dependencies[0].to, "C");
    }
}
