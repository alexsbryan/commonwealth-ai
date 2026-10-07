"""Host validation for the JSON-schema subset used by this experiment."""


def problems(value, schema, path="$"):
    if "oneOf" in schema:
        matches = sum(not problems(value, arm, path) for arm in schema["oneOf"])
        return [] if matches == 1 else [f"{path}: must match exactly one legal action"]
    bad = []
    if "const" in schema and value != schema["const"]:
        bad.append(f"{path}: wrong constant")
    if "enum" in schema and value not in schema["enum"]:
        bad.append(f"{path}: outside the permitted set")
    kind = schema.get("type")
    checks = {"object": lambda: isinstance(value, dict),
              "array": lambda: isinstance(value, list),
              "string": lambda: isinstance(value, str),
              "boolean": lambda: type(value) is bool,
              "integer": lambda: type(value) is int}
    if kind and (kind not in checks or not checks[kind]()):
        return bad + [f"{path}: expected {kind}"]
    if kind == "object":
        props = schema.get("properties", {})
        bad += [f"{path}.{key}: missing" for key in schema.get("required", []) if key not in value]
        if schema.get("additionalProperties") is False:
            bad += [f"{path}.{key}: unknown" for key in value if key not in props]
        for key in value.keys() & props.keys():
            bad += problems(value[key], props[key], f"{path}.{key}")
    if kind in ("array", "string"):
        lo, hi = ("minItems", "maxItems") if kind == "array" else ("minLength", "maxLength")
        if len(value) < schema.get(lo, 0) or len(value) > schema.get(hi, float("inf")):
            bad.append(f"{path}: length outside bounds")
    if kind == "array":
        for i, item in enumerate(value):
            bad += problems(item, schema.get("items", {}), f"{path}[{i}]")
    if kind == "integer":
        if value < schema.get("minimum", float("-inf")) or value > schema.get("maximum", float("inf")):
            bad.append(f"{path}: integer outside bounds")
    return bad
