// SPDX-License-Identifier: AGPL-3.0-or-later
//! Template resolution — the workflow sibling of `executor::resolve_inputs`.
//! Substitutes `{step_id.key}` and `{item.field}` against the per-item `Scope`,
//! preserving the glassbox warn-on-missing-key behaviour (ARCH §9).

use std::sync::OnceLock;

use regex::Regex;
use sovereign_core::types::StepOutput;

use crate::model::{ResolvedArgs, Scope, StepSpec};

/// `{<ident>.<ident>}` — a reference to a step output (`{read.output}`) or an
/// item field (`{item.path}`).
fn ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\.([A-Za-z0-9_]+)\}").unwrap())
}

/// Step ids referenced by `text` (the `ref` of `{ref.key}`), excluding the
/// `item` pseudo-source. Drives DAG-edge derivation.
pub fn referenced_ids(text: &str) -> Vec<String> {
    ref_re()
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .filter(|r| r != "item")
        .collect()
}

/// Resolve every `{ref.key}` in `template` against `scope`.
pub fn resolve_str(template: &str, scope: &Scope) -> String {
    ref_re()
        .replace_all(template, |caps: &regex::Captures| {
            resolve_one(&caps[1], &caps[2], scope)
        })
        .into_owned()
}

fn resolve_one(reference: &str, key: &str, scope: &Scope) -> String {
    if reference == "item" {
        return scope.item.get(key).cloned().unwrap_or_default();
    }
    let Some(artifact) = scope.completed.get(reference) else {
        tracing::warn!(
            reference, key,
            "resolve: reference to an unknown/incomplete step — empty string"
        );
        return String::new();
    };
    extract(&artifact.output, key, reference)
}

fn extract(output: &StepOutput, key: &str, reference: &str) -> String {
    match output {
        StepOutput::Text(s) => s.clone(),
        StepOutput::Json(v) => {
            if key == "output" {
                serde_json::to_string_pretty(v).unwrap_or_default()
            } else {
                match v.get(key) {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => {
                        tracing::warn!(
                            from = reference, key,
                            available = ?v.as_object().map(|o| o.keys().collect::<Vec<_>>()),
                            "resolve: key not in upstream Json output — empty string \
                             (check the tool's output_schema)"
                        );
                        String::new()
                    }
                }
            }
        }
        StepOutput::ReasonWithToolsResult { text, .. } => text.clone(),
        StepOutput::Jump(_) | StepOutput::Skipped => String::new(),
    }
}

/// Resolve a `toml::Value` (a step's `params`) into a `serde_json::Value`,
/// substituting templates inside every string.
pub fn resolve_value(v: &toml::Value, scope: &Scope) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(resolve_str(s, scope)),
        toml::Value::Integer(i) => serde_json::Value::from(*i),
        toml::Value::Float(f) => serde_json::Value::from(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(|x| resolve_value(x, scope)).collect())
        }
        toml::Value::Table(t) => serde_json::Value::Object(
            t.iter().map(|(k, x)| (k.clone(), resolve_value(x, scope))).collect(),
        ),
    }
}

/// Resolve a step's templated fields against the scope.
pub fn resolve_args(spec: &StepSpec, scope: &Scope) -> ResolvedArgs {
    ResolvedArgs {
        prompt: spec.prompt.as_ref().map(|p| resolve_str(p, scope)),
        system: spec.system.as_ref().map(|s| resolve_str(s, scope)),
        input: spec.input.as_ref().map(|i| resolve_str(i, scope)),
        params: spec.params.as_ref().map(|p| resolve_value(p, scope)),
    }
}
