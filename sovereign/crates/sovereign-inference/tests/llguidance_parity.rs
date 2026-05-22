//! Parity fixtures for the JsonConstraint → llguidance migration.
//!
//! Background: `LLGUIDANCE_MIGRATION_AUDIT.md` (§4 fixture plan).
//!
//! Goal: pin invariants that gate the D-full migration BEFORE the
//! wiring PR lands. Each fixture corresponds to a schema actively
//! used in `sovereign-core` or `sovereign-cli` today (see audit §2
//! inventory) and exercises one risk class (audit §3).
//!
//! Approach: drive a `Matcher` against `ApproximateTokEnv::
//! single_byte_env()` so every printable ASCII byte is its own token
//! id. Feed bytes one at a time, asserting which next-byte choices
//! the mask permits or rejects. Mirrors the proven pattern in
//! `llguidance_constraint::tests`.
//!
//! Perf benches (§4 #6, #7) live in `examples/bench_*.rs` because
//! they require a real `LlamaModel` / GGUF on disk; placeholder
//! markers below document the intent.

use llguidance::{
    api::TopLevelGrammar,
    toktrie::ApproximateTokEnv,
    Matcher, ParserFactory,
};
use sovereign_inference::llguidance_constraint::default_additional_properties_false;

// ─── helpers ───────────────────────────────────────────────────────────

fn matcher_for(schema: serde_json::Value) -> Matcher {
    let tok_env = ApproximateTokEnv::single_byte_env();
    let factory = ParserFactory::new_simple(&tok_env).expect("factory");
    let grammar = TopLevelGrammar::from_json_schema(schema);
    let parser = factory.create_parser(grammar);
    let m = Matcher::new(parser);
    assert!(
        !m.is_error(),
        "schema grammar must compile: {:?}",
        m.get_error()
    );
    m
}

/// Feed a byte string into the matcher, asserting every byte is
/// allowed at its position. Stops on first masked byte (panics with
/// position + offending byte). Use this for the "happy path" half of
/// each fixture before exercising the rejection path.
fn consume_ok(m: &mut Matcher, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        let mask = m.compute_mask().expect("compute_mask");
        assert!(
            mask.is_allowed(b as u32),
            "byte #{i} {:?} should be allowed; emitted prefix: {}",
            b as char,
            String::from_utf8_lossy(&bytes[..i])
        );
        m.consume_token(b as u32).expect("consume");
    }
}

/// Returns true iff the byte is allowed by the current next-token mask.
fn allows(m: &mut Matcher, b: u8) -> bool {
    let mask = m.compute_mask().expect("compute_mask");
    mask.is_allowed(b as u32)
}

// ─── §3.A walker — additionalProperties default ────────────────────────

#[test]
fn unit_default_additional_properties_walker_injects_on_typed_object() {
    let mut s = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } },
        "required": ["x"]
    });
    default_additional_properties_false(&mut s);
    assert_eq!(s["additionalProperties"], serde_json::Value::Bool(false));
}

#[test]
fn unit_walker_preserves_explicit_true() {
    let mut s = serde_json::json!({
        "type": "object",
        "additionalProperties": true,
        "properties": { "x": { "type": "string" } }
    });
    default_additional_properties_false(&mut s);
    assert_eq!(s["additionalProperties"], serde_json::Value::Bool(true));
}

#[test]
fn unit_walker_recurses_nested_objects_arrays_oneof_defs() {
    let mut s = serde_json::json!({
        "type": "object",
        "properties": {
            "nested_obj": {
                "type": "object",
                "properties": { "a": { "type": "string" } }
            },
            "array_of_obj": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "b": { "type": "string" } }
                }
            },
            "variant": {
                "oneOf": [
                    { "type": "object", "properties": { "c": { "type": "string" } } },
                    { "type": "object", "properties": { "d": { "type": "string" } } }
                ]
            }
        },
        "$defs": {
            "Foo": { "type": "object", "properties": { "e": { "type": "string" } } }
        }
    });
    default_additional_properties_false(&mut s);
    // Outer object.
    assert_eq!(s["additionalProperties"], serde_json::Value::Bool(false));
    // Nested under properties.
    assert_eq!(
        s["properties"]["nested_obj"]["additionalProperties"],
        serde_json::Value::Bool(false)
    );
    // Inside an array's items.
    assert_eq!(
        s["properties"]["array_of_obj"]["items"]["additionalProperties"],
        serde_json::Value::Bool(false)
    );
    // Inside a oneOf branch.
    assert_eq!(
        s["properties"]["variant"]["oneOf"][0]["additionalProperties"],
        serde_json::Value::Bool(false)
    );
    assert_eq!(
        s["properties"]["variant"]["oneOf"][1]["additionalProperties"],
        serde_json::Value::Bool(false)
    );
    // Inside $defs.
    assert_eq!(
        s["$defs"]["Foo"]["additionalProperties"],
        serde_json::Value::Bool(false)
    );
}

#[test]
fn unit_walker_ignores_non_object_subtrees() {
    let mut s = serde_json::json!({
        "type": "string",
        "enum": ["a", "b"]
    });
    let before = s.clone();
    default_additional_properties_false(&mut s);
    assert_eq!(s, before, "non-object schema must pass through unchanged");
}

// ─── §3.A end-to-end: walker injection actually masks extra fields ─────

#[test]
fn parity_titles_array_extra_field_masked_after_walker() {
    // Audit §2 row #1 — runtime.rs:4162 title-expansion schema.
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "titles": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "maxItems": 3
            }
        },
        "required": ["titles"]
    });
    default_additional_properties_false(&mut schema);

    let mut m = matcher_for(schema);
    // Open object + titles field + value array.
    consume_ok(&mut m, br#"{"titles":["A"]"#);
    // Comma here would open an extra field — must be masked, because
    // the walker just turned additionalProperties:false on. The only
    // legal next-byte is `}` (close root).
    assert!(
        !allows(&mut m, b','),
        "comma must be masked: extra fields forbidden under additionalProperties:false"
    );
    assert!(allows(&mut m, b'}'), "close-brace must be allowed");
}

// ─── §3.B silent bounds suddenly enforced ──────────────────────────────

#[test]
fn parity_titles_minitems_enforced_under_llguidance() {
    // minItems was silently dropped by JsonConstraint. Under
    // llguidance it must be real. With minItems:1, an empty array
    // must NOT satisfy the schema — the `]` byte after `[` should be
    // masked while count < 1.
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "titles": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "maxItems": 3
            }
        },
        "required": ["titles"]
    });
    default_additional_properties_false(&mut schema);

    let mut m = matcher_for(schema);
    consume_ok(&mut m, br#"{"titles":["#);
    // Zero items so far. Closing the array now would violate
    // minItems:1.
    assert!(
        !allows(&mut m, b']'),
        "close-bracket must be masked while count < minItems"
    );
    // After one item, close-bracket is legal.
    consume_ok(&mut m, br#""A""#);
    assert!(
        allows(&mut m, b']'),
        "close-bracket must be allowed once minItems satisfied"
    );
}

#[test]
fn parity_essay_readiness_integer_bounds_enforced() {
    // Audit §2 row #10 — score.rs:780-783 essay rubric. Integer with
    // minimum:0 / maximum:3. Verify llguidance masks `5` and friends.
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "argument_depth": { "type": "integer", "minimum": 0, "maximum": 3 }
        },
        "required": ["argument_depth"]
    });
    default_additional_properties_false(&mut schema);

    let mut m = matcher_for(schema);
    consume_ok(&mut m, br#"{"argument_depth":"#);
    // Allowed: 0,1,2,3. Masked: 4-9.
    for d in b'0'..=b'3' {
        assert!(allows(&mut m, d), "digit {} must be allowed", d as char);
    }
    for d in b'4'..=b'9' {
        assert!(!allows(&mut m, d), "digit {} must be masked (above maximum)", d as char);
    }
}

// ─── §3.A + nested type union ─────────────────────────────────────────

#[test]
fn parity_thread_judge_type_union_accepts_integer_and_null() {
    // Audit §2 row #11 — runner_threads.rs:450 evidence_turn field
    // typed as ["integer", "null"]. Walk far enough into the schema
    // to verify both branches are reachable at the value position.
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "evidence_turn": { "type": ["integer", "null"] }
        },
        "required": ["evidence_turn"]
    });
    default_additional_properties_false(&mut schema);

    let mut m = matcher_for(schema);
    consume_ok(&mut m, br#"{"evidence_turn":"#);
    // Either a digit (integer branch) or `n` (null literal) must be
    // reachable from this state.
    assert!(allows(&mut m, b'0'), "digit must be reachable (integer branch)");
    assert!(allows(&mut m, b'n'), "`n` must be reachable (null branch)");
}

// ─── §1.1 enum drop-in ────────────────────────────────────────────────

#[test]
fn parity_intent_enum_router_only_emits_listed_values() {
    // Audit §2 row #6 — router.rs:1438 intent classifier. After
    // `{"intent":"`, only the first letter of one of the 9 enum
    // values may follow. Sample-check: S (SIMPLE), L (LOOKUP), C
    // (COMPARISON / CONATION / COMMISSION), R (REASONING), A (ACTION),
    // E (EXPRESSIVE), M (METALINGUAL). Everything else masked.
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "intent": {
                "type": "string",
                "enum": [
                    "SIMPLE","LOOKUP","COMPARISON","REASONING","ACTION",
                    "CONATION","COMMISSION","EXPRESSIVE","METALINGUAL"
                ]
            }
        },
        "required": ["intent"]
    });
    default_additional_properties_false(&mut schema);

    let mut m = matcher_for(schema);
    consume_ok(&mut m, br#"{"intent":""#);
    // First-letter set of the enum values: S, L, C, R, A, E, M.
    for c in [b'S', b'L', b'C', b'R', b'A', b'E', b'M'] {
        assert!(allows(&mut m, c), "first-letter {} must be allowed", c as char);
    }
    // Non-starter letters must be masked.
    for c in [b'X', b'Z', b'B', b'D'] {
        assert!(
            !allows(&mut m, c),
            "letter {} not a valid enum first-letter, must be masked",
            c as char
        );
    }
}

// ─── §3.D tool envelope oneOf + dynamic patterns ──────────────────────

#[test]
fn parity_tool_envelope_oneof_with_cmd_prefix() {
    // Audit §2 dynamic row + §3.D. Tool envelope is `oneOf` over
    // per-tool objects; the `cmd` field of one tool carries a
    // `pattern: "^cargo "` to force the literal prefix.
    // Verify llguidance enforces the literal byte-by-byte after the
    // model reaches the `cmd` value position.
    let mut schema = serde_json::json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "enum": ["bash"] },
                    "arguments": {
                        "type": "object",
                        "properties": {
                            "cmd": { "type": "string", "pattern": "^cargo " }
                        },
                        "required": ["cmd"],
                        "additionalProperties": false
                    }
                },
                "required": ["name", "arguments"],
                "additionalProperties": false
            }
        ]
    });
    // Walker is a no-op at the root (oneOf, not type:object); the
    // inner objects already set additionalProperties:false explicitly
    // (mirrors inference_adapter.rs:881). Run anyway to confirm
    // idempotence.
    default_additional_properties_false(&mut schema);

    let mut m = matcher_for(schema);
    consume_ok(&mut m, br#"{"name":"bash","arguments":{"cmd":""#);
    // Literal prefix "cargo " must be forced one byte at a time.
    assert!(allows(&mut m, b'c'), "first prefix byte `c` must be allowed");
    // Any other first byte must be masked.
    for c in [b'l', b'r', b'g', b'x', b'C'] {
        assert!(
            !allows(&mut m, c),
            "byte {} must be masked while in literal-prefix position",
            c as char
        );
    }
    // Consume `c`, then `a` is forced.
    m.consume_token(b'c' as u32).expect("consume c");
    assert!(allows(&mut m, b'a'), "second byte `a` must be allowed");
    assert!(!allows(&mut m, b'o'), "non-prefix byte `o` must be masked");
}

// ─── §3.A end-to-end via from_schema_value ────────────────────────────

#[test]
fn parity_from_schema_value_applies_walker_then_compiles() {
    // Schema without explicit additionalProperties — the
    // `from_schema_value` helper must inject false before passing to
    // llguidance, so the resulting grammar masks extra fields.
    use sovereign_inference::llguidance_constraint::default_additional_properties_false;

    let raw_schema = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "string" } },
        "required": ["x"]
    });

    // Mirror what `from_schema_value` does internally (so the test
    // doesn't require a real LlamaModel to inspect the post-walker
    // schema bytes).
    let mut walked = raw_schema.clone();
    default_additional_properties_false(&mut walked);

    let mut m = matcher_for(walked);
    consume_ok(&mut m, br#"{"x":"a""#);
    // After the value closes, the only legal next-byte is `}` (close
    // root). `,` would attempt to open an extra field — masked.
    assert!(
        !allows(&mut m, b','),
        "extra-field comma must be masked when walker injected additionalProperties:false"
    );
    assert!(allows(&mut m, b'}'), "close-brace must be allowed");
}

// ─── env gate parser (matches embedded::full_llguidance_enabled_from_env)

/// Local mirror of `full_llguidance_enabled_from_env` from
/// `embedded.rs`. The fn is `pub(crate)` over there (private outside
/// the module). Test the contract here so the env-gate parsing
/// behaviour is pinned by a public test that documents intent for
/// future maintainers.
///
/// Contract: ONLY `"1"` or case-insensitive `"true"` enables the
/// gate. Empty string, missing var, "0", "false", "yes", "no",
/// random garbage — all return `false`. Mirrors the
/// `jump_fwd_enabled_from_env` shape upstream.
fn env_gate_parser(env_get: impl Fn(&str) -> Option<String>) -> bool {
    match env_get("SOVEREIGN_FULL_LLGUIDANCE") {
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        None => false,
    }
}

#[test]
fn env_gate_off_by_default_when_var_missing() {
    assert!(!env_gate_parser(|_| None));
}

#[test]
fn env_gate_on_with_literal_one() {
    assert!(env_gate_parser(|k| {
        if k == "SOVEREIGN_FULL_LLGUIDANCE" { Some("1".into()) } else { None }
    }));
}

#[test]
fn env_gate_on_with_case_insensitive_true() {
    for v in ["true", "True", "TRUE", "tRuE"] {
        assert!(
            env_gate_parser(|k| {
                if k == "SOVEREIGN_FULL_LLGUIDANCE" { Some(v.into()) } else { None }
            }),
            "value {v:?} must enable the gate"
        );
    }
}

#[test]
fn env_gate_off_with_falsy_and_garbage_values() {
    for v in ["0", "false", "False", "no", "yes", "", "garbage"] {
        assert!(
            !env_gate_parser(|k| {
                if k == "SOVEREIGN_FULL_LLGUIDANCE" { Some(v.into()) } else { None }
            }),
            "value {v:?} must NOT enable the gate (only `1`/`true` do)"
        );
    }
}

// ─── §4 perf benches — superseded by `examples/bench_constraint.rs` ───
//
// The two perf questions (decode tok/s parity + `compute_ff_tokens`
// yield) now have a real runnable harness:
//
//   cargo run --release -p sovereign-inference --example bench_constraint -- \
//       --model <gguf> --engine both --iters 5 --gen-tokens 200
//
// First smoke run (Qwen3.5-2B Metal, 2026-05-22): llguidance was
// 2.5× faster on decode tok/s and `ff_yield = 0.00` (every
// `compute_ff_tokens` call returned empty under `ApproximateTokEnv`).
// See `LLGUIDANCE_MIGRATION_AUDIT.md` §3.C/§3.G smoke note.
//
// The `bench all --synth` regression gate (audit §4.2) is the
// canonical end-to-end test.
