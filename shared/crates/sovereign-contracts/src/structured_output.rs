// SPDX-License-Identifier: AGPL-3.0-or-later
//! The closed-object default for `structured_output` schemas, applied by the
//! remote client before it sends one (`oicp_client`
//! `chat_wire::write_structured_output`); the embedded engine that also
//! applied it was deleted (es-g2). One decider, so a schema means one
//! language whichever host compiles it.

use serde_json::Value;

/// Walk a JSON-Schema document and inject `additionalProperties: false` on
/// every typed-object node that declares properties and does not set the
/// field itself.
///
/// **Why this exists.** The in-house `JsonConstraint::compile_schema`
/// defaulted `additionalProperties` to `false` for `type: object` (non-spec:
/// JSON Schema defaults to `true`), and callers across `sovereign-core` rely
/// on that strictness rather than set the field.
/// llguidance's `TopLevelGrammar::from_json_schema` follows the spec, and so
/// does a llama-server, so the default is kept here, at the boundary, rather
/// than at every call site. A subtree that sets `additionalProperties: true`
/// keeps it.
///
/// **An object that declares no properties stays open.** Closing
/// `{"type": "object"}` leaves exactly one value, `{}`: the response_format
/// `json_object` the serving host maps to that schema answered `{\n \t}` to
/// "an object with key 'answer'" (ledger C7, case 612c860d26ac). Nothing
/// was declared, so nothing extra can be excluded.
///
/// Recurses into `properties[*]`, `items`, `additionalProperties` (when a
/// schema), `anyOf[*]`, `oneOf[*]`, `allOf[*]`, `$defs[*]`, `definitions[*]`.
/// Non-object values pass through unchanged.
pub fn default_additional_properties_false(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    let is_typed_object = obj.get("type").and_then(Value::as_str) == Some("object");
    let declares_properties = obj
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|p| !p.is_empty());

    if is_typed_object && declares_properties && !obj.contains_key("additionalProperties") {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    }

    if let Some(props) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
        for sub in props.values_mut() {
            default_additional_properties_false(sub);
        }
    }
    if let Some(items) = obj.get_mut("items") {
        default_additional_properties_false(items);
    }
    if let Some(ap) = obj.get_mut("additionalProperties") {
        if ap.is_object() {
            default_additional_properties_false(ap);
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = obj.get_mut(key).and_then(|v| v.as_array_mut()) {
            for sub in arr.iter_mut() {
                default_additional_properties_false(sub);
            }
        }
    }
    for key in ["$defs", "definitions"] {
        if let Some(defs) = obj.get_mut(key).and_then(|v| v.as_object_mut()) {
            for sub in defs.values_mut() {
                default_additional_properties_false(sub);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn walked(mut schema: Value) -> Value {
        default_additional_properties_false(&mut schema);
        schema
    }

    #[test]
    fn a_declared_object_is_closed_at_every_depth() {
        let schema = walked(json!({
            "type": "object",
            "properties": {
                "inner": {"type": "object", "properties": {"x": {"type": "string"}}},
                "list": {"type": "array", "items": {"type": "object", "properties": {"y": {}}}}
            }
        }));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(
            schema["properties"]["inner"]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            schema["properties"]["list"]["items"]["additionalProperties"],
            json!(false)
        );
    }

    #[test]
    fn an_explicit_setting_stands() {
        let open = json!({"type": "object", "properties": {"x": {}}, "additionalProperties": true});
        assert_eq!(walked(open.clone()), open);
    }

    /// THE FAILING INPUT (C7): `json_object` arrives as `{"type": "object"}`,
    /// and closed it admitted `{}` alone. With no properties declared it
    /// stays open, at the top and nested.
    #[test]
    fn an_object_that_declares_no_properties_stays_open() {
        for bare in [
            json!({"type": "object"}),
            json!({"type": "object", "properties": {}}),
        ] {
            assert_eq!(walked(bare.clone()), bare);
        }
        let nested = walked(json!({
            "type": "object",
            "properties": {"meta": {"type": "object"}}
        }));
        assert_eq!(nested["additionalProperties"], json!(false));
        assert!(nested["properties"]["meta"]
            .get("additionalProperties")
            .is_none());
    }
}
