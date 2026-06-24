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
    // The key may be dotted (`a.b.c`) so a `{element.chapter.id}` reaches a
    // nested field; the leading reference itself stays a single identifier.
    RE.get_or_init(|| Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\.([A-Za-z0-9_][A-Za-z0-9_.]*)\}").unwrap())
}

/// Step ids referenced by `text` (the `ref` of `{ref.key}`), excluding the
/// `item` and `element` pseudo-sources. Drives DAG-edge derivation (a
/// `for_each` step's collection dep is declared separately, not inferred from
/// `{element.…}`).
pub fn referenced_ids(text: &str) -> Vec<String> {
    ref_re()
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .filter(|r| r != "item" && r != "element")
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
    if reference == "element" {
        // The current `for_each` element: `{element.<field>}` for an object,
        // `{element.value}` for a scalar.
        return scope
            .element
            .as_ref()
            .and_then(|e| element_field(e, key))
            .unwrap_or_default();
    }
    let Some(artifact) = scope.completed.get(reference) else {
        tracing::warn!(
            reference, key,
            "resolve: reference to an unknown/incomplete step — empty string"
        );
        return String::new();
    };
    // `{step.failures}` is the tolerant-`for_each` failure record, sitting beside
    // the output on the artifact (not inside it).
    if key == "failures" {
        return serde_json::to_string_pretty(&artifact.failures).unwrap_or_default();
    }
    extract(&artifact.output, key, reference)
}

fn extract(output: &StepOutput, key: &str, reference: &str) -> String {
    match output {
        StepOutput::Text(s) => s.clone(),
        StepOutput::Json(v) => {
            if key == "output" {
                serde_json::to_string_pretty(v).unwrap_or_default()
            } else {
                match get_path(v, key) {
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

/// `{element.<field>}` (or dotted `{element.a.b}`) for an object element;
/// `{element.value}` for a scalar.
fn element_field(value: &serde_json::Value, key: &str) -> Option<String> {
    if key == "value" {
        return Some(value_to_string(value));
    }
    get_path(value, key).map(value_to_string)
}

/// Traverse a dotted path (`a.b.c`) into a JSON value — so `{element.chapter.id}`
/// reaches a nested field a `for_each` chain carries.
pub(crate) fn get_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    let mut cur = value;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Resolve a `toml::Value` (a step's `params`) into a `serde_json::Value`,
/// substituting templates inside every string.
pub fn resolve_value(v: &toml::Value, scope: &Scope) -> serde_json::Value {
    match v {
        toml::Value::String(s) => resolve_value_string(s, scope),
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

/// A templated string in JSON *value* position (a `params`/`stamp` field), as
/// opposed to a prose field (`prompt`/`input`, resolved by `resolve_str`). When
/// the whole string is a lone reference to a completed *step's* whole output
/// (`"{atoms.output}"`), splice that artifact's JSON value — so a workflow can
/// nest one step's structured output inside another's object (assemble an
/// envelope: `questions_by_chapter = "{atoms.output}"`) rather than embedding a
/// stringified copy. Anything else — an interpolated string, a sub-field ref, or
/// an `{item.*}`/`{element.*}` ref — keeps string semantics (so a `chapter_id`
/// stamped from `{element.index}` stays the string "0", matching the real
/// `ExtractedQuestion.chapter_id: String`). This is the *only* value position
/// that differs from stringification, and only for a JSON-valued step output.
fn resolve_value_string(s: &str, scope: &Scope) -> serde_json::Value {
    // A bare `{element}` ref splices the WHOLE current for_each element (object
    // or array) — so a for_each directly over a collection of structs can pass
    // each struct by value to a tool (e.g. `cluster = "{element}"`). A scalar
    // element falls through to string semantics, matching the sub-field rule.
    if s == "{element}" {
        if let Some(v) = scope.element.as_ref() {
            if v.is_object() || v.is_array() {
                return v.clone();
            }
        }
    }
    if let Some((reference, key)) = lone_ref(s) {
        if reference == "element" {
            // A `for_each` element's OBJECT/ARRAY sub-field splices as a value —
            // so a for_each *chain* can thread structured per-element data (a
            // chapter, a prompt's schema) from one step to the next. A SCALAR
            // sub-field keeps string semantics, so `{element.index}` stays the
            // string "0" (the stamp/chapter-id contract).
            if let Some(v) = scope.element.as_ref().and_then(|e| get_path(e, key)) {
                if v.is_object() || v.is_array() {
                    return v.clone();
                }
            }
        } else if reference != "item" {
            if let Some(artifact) = scope.completed.get(reference) {
                // The tolerant-`for_each` failures array sits beside the output.
                if key == "failures" {
                    return serde_json::Value::Array(artifact.failures.clone());
                }
                // Splice the whole structured output (`output`) or any of its
                // sub-fields (`{compose.schema}` → the schema object) AS A VALUE,
                // so a composed `model:` step can take a prior step's
                // system/user/schema by reference. A missing key / non-Json output
                // falls through to string semantics below.
                match &artifact.output {
                    StepOutput::Json(v) => {
                        if key == "output" {
                            return v.clone();
                        }
                        if let Some(sub) = v.get(key) {
                            return sub.clone();
                        }
                    }
                    other => {
                        if key == "output" {
                            return splice_output(other);
                        }
                    }
                }
            }
        }
    }
    serde_json::Value::String(resolve_str(s, scope))
}

/// `Some((ref, key))` iff `s` is exactly `{ref.key}` and nothing surrounds it.
fn lone_ref(s: &str) -> Option<(&str, &str)> {
    let inner = s.strip_prefix('{')?.strip_suffix('}')?;
    if inner.contains('{') || inner.contains('}') {
        return None;
    }
    let (r, k) = inner.split_once('.')?;
    (!r.is_empty() && !k.is_empty()).then_some((r, k))
}

/// The spliced JSON value of a step's whole output. A text output becomes a JSON
/// string — identical to the old stringified behaviour, so only a JSON-valued
/// output (an array/object) actually changes (it nests as structure, not a
/// stringified copy).
fn splice_output(output: &StepOutput) -> serde_json::Value {
    match output {
        StepOutput::Json(v) => v.clone(),
        StepOutput::Text(s) => serde_json::Value::String(s.clone()),
        StepOutput::ReasonWithToolsResult { text, .. } => serde_json::Value::String(text.clone()),
        StepOutput::Jump(_) | StepOutput::Skipped => serde_json::Value::Null,
    }
}

/// Resolve a step's templated fields against the scope.
pub fn resolve_args(spec: &StepSpec, scope: &Scope) -> ResolvedArgs {
    ResolvedArgs {
        prompt: spec.prompt.as_ref().map(|p| resolve_str(p, scope)),
        system: spec.system.as_ref().map(|s| resolve_str(s, scope)),
        system_file: spec.system_file.as_ref().map(|p| resolve_str(p, scope)),
        input: spec.input.as_ref().map(|i| resolve_str(i, scope)),
        params: spec.params.as_ref().map(|p| resolve_value(p, scope)),
        structured_output: spec.structured_output.as_ref().map(|v| resolve_value(v, scope)),
        grammar: spec.grammar.as_ref().map(|g| resolve_str(g, scope)),
        stamp: spec.stamp.as_ref().map(|v| resolve_value(v, scope)),
        raw_output: spec.raw_output.unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use sovereign_core::types::StepOutput;

    use super::resolve_value;
    use crate::model::{Artifact, Scope};

    fn scope_with_step(id: &str, output: StepOutput) -> Scope {
        let mut completed = BTreeMap::new();
        completed.insert(id.to_string(), Artifact::new("x", output));
        Scope {
            item: BTreeMap::new(),
            completed: Arc::new(completed),
            element: None,
        }
    }

    /// Value-splicing semantics: a lone `{step.output}` ref in a value position
    /// splices the JSON value (so an envelope nests the collection as structure);
    /// an interpolated ref, and `{item.*}`/`{element.*}` refs, stay strings.
    #[test]
    fn lone_step_output_ref_splices_value_others_stringify() {
        let arr = serde_json::json!([{ "chapter_id": "0", "questions": ["q"] }]);
        let scope = scope_with_step("atoms", StepOutput::Json(arr.clone()));

        let params: toml::Value = toml::from_str(
            "schema_version = 1\n\
             questions_by_chapter = \"{atoms.output}\"\n\
             note = \"see {atoms.output} for detail\"",
        )
        .unwrap();
        let env = resolve_value(&params, &scope);

        assert_eq!(env.get("schema_version").unwrap(), &serde_json::json!(1));
        // Lone whole-output ref -> the nested ARRAY, not a stringified copy.
        assert_eq!(
            env.get("questions_by_chapter").unwrap(),
            &arr,
            "a lone {{step.output}} ref splices the value"
        );
        // Interpolated ref -> a string (the output stringified into prose).
        assert!(
            env.get("note").unwrap().is_string(),
            "an interpolated ref stays a string"
        );

        // An `{item.*}` ref never splices, even when lone.
        let mut item = BTreeMap::new();
        item.insert("stem".to_string(), "doc".to_string());
        let s = Scope {
            item,
            completed: Arc::new(BTreeMap::new()),
            element: None,
        };
        assert_eq!(
            resolve_value(&toml::Value::String("{item.stem}".into()), &s),
            serde_json::json!("doc"),
            "an {{item.*}} ref stays a string"
        );
    }

    /// A `for_each` element's object/array sub-field splices as a value (threading
    /// structured per-element data through a for_each chain); a scalar sub-field
    /// stays a string (preserving the `{element.index}` → "0" stamp contract).
    #[test]
    fn element_object_subfield_splices_scalar_stringifies() {
        let mut scope = Scope::default();
        scope.element = Some(serde_json::json!({
            "chapter": { "text": "hi", "id": "sec_0001" },
            "tags": ["a", "b"],
            "index": 0
        }));
        let params: toml::Value = toml::from_str(
            "ch = \"{element.chapter}\"\ntags = \"{element.tags}\"\nidx = \"{element.index}\"",
        )
        .unwrap();
        let out = resolve_value(&params, &scope);
        assert_eq!(
            out["ch"],
            serde_json::json!({ "text": "hi", "id": "sec_0001" }),
            "an object sub-field splices"
        );
        assert_eq!(out["tags"], serde_json::json!(["a", "b"]), "an array sub-field splices");
        assert_eq!(out["idx"], serde_json::json!("0"), "a scalar sub-field stays a string");

        // Dotted access reaches a nested field (e.g. stamping chapter_id from a
        // zipped {result, chapter} element).
        let nested: toml::Value = toml::from_str("id = \"{element.chapter.id}\"").unwrap();
        assert_eq!(resolve_value(&nested, &scope)["id"], serde_json::json!("sec_0001"));
    }

    /// A bare `{element}` ref splices the WHOLE element by value — so a for_each
    /// directly over a collection of structs can pass each struct to a tool.
    #[test]
    fn bare_element_ref_splices_whole_element() {
        let mut scope = Scope::default();
        scope.element = Some(serde_json::json!({ "id": "cl_0001", "facet": "question" }));
        let params: toml::Value = toml::from_str("cluster = \"{element}\"").unwrap();
        assert_eq!(
            resolve_value(&params, &scope)["cluster"],
            serde_json::json!({ "id": "cl_0001", "facet": "question" }),
            "the whole element object splices by value"
        );
    }
}
