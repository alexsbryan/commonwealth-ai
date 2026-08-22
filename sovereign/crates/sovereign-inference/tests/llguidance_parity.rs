// SPDX-License-Identifier: AGPL-3.0-or-later
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

use llguidance::{api::TopLevelGrammar, toktrie::ApproximateTokEnv, Matcher, ParserFactory};
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
        assert!(
            !allows(&mut m, d),
            "digit {} must be masked (above maximum)",
            d as char
        );
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
    assert!(
        allows(&mut m, b'0'),
        "digit must be reachable (integer branch)"
    );
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
        assert!(
            allows(&mut m, c),
            "first-letter {} must be allowed",
            c as char
        );
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
    assert!(
        allows(&mut m, b'c'),
        "first prefix byte `c` must be allowed"
    );
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
        if k == "SOVEREIGN_FULL_LLGUIDANCE" {
            Some("1".into())
        } else {
            None
        }
    }));
}

#[test]
fn env_gate_on_with_case_insensitive_true() {
    for v in ["true", "True", "TRUE", "tRuE"] {
        assert!(
            env_gate_parser(|k| {
                if k == "SOVEREIGN_FULL_LLGUIDANCE" {
                    Some(v.into())
                } else {
                    None
                }
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
                if k == "SOVEREIGN_FULL_LLGUIDANCE" {
                    Some(v.into())
                } else {
                    None
                }
            }),
            "value {v:?} must NOT enable the gate (only `1`/`true` do)"
        );
    }
}

// ─── The silent-fallback triggers (2026-08-19) ─────────────────────────
//
// `build_sampler` used to swallow a compile failure and decode
// free-form behind a WARN, so a schema in ANY of the shapes below
// produced a 200 whose text obeyed no constraint at all — the §18.3
// failure where the label, the shape and the finish reason are all
// correct and no client can tell. It now refuses the request.
//
// These tests pin the TRIGGER: they name the inputs that reach the
// refusal, and each pairs with a twin that must still compile, so a
// change which simply broke schema compilation everywhere would turn
// them red rather than green (§18.1 — a check with no failing input
// you can name is not a check).
//
// The `oneOf` case is not hypothetical. `gym/comaintainer/markers.py`
// records it live on 2026-08-10: "a oneOf whose branches declare only
// properties/required is DROPPED by the daemon silently."

/// Does this schema produce a usable grammar, or does llguidance
/// refuse it? Mirrors `matcher_for` without its compile assertion.
fn compiles(schema: serde_json::Value) -> bool {
    let tok_env = ApproximateTokEnv::single_byte_env();
    let Ok(factory) = ParserFactory::new_simple(&tok_env) else {
        return false;
    };
    let grammar = TopLevelGrammar::from_json_schema(schema);
    let parser = factory.create_parser(grammar);
    !Matcher::new(parser).is_error()
}

#[test]
fn unimplemented_keyword_does_not_compile() {
    // llguidance hard-errors on keywords outside its implemented set
    // rather than ignoring them. Before the fix this became free-form
    // sampling; now it becomes a refusal.
    assert!(
        !compiles(serde_json::json!({
            "type": "array",
            "items": {"type": "string"},
            "uniqueItems": true
        })),
        "`uniqueItems` is not implemented by llguidance — if this now \
         compiles, the refusal path in build_sampler is unreachable for \
         it and this test is the only thing that would have said so"
    );
    // Twin: the same schema minus the unimplemented keyword must
    // compile, or the assertion above proves nothing.
    assert!(
        compiles(serde_json::json!({
            "type": "array",
            "items": {"type": "string"}
        })),
        "the twin without `uniqueItems` must still compile"
    );
}

#[test]
fn multi_branch_oneof_needs_a_discriminator_to_compile() {
    // `oneOf` survives only when llguidance can prove the branches
    // disjoint (`coerce_one_of`/`lenient` are both off and never set).
    // A `const` discriminator on a required key is what supplies that
    // proof — this is exactly the shape `markers.verdict_schema()`
    // relies on, and losing it disarms the whole constraint.
    let discriminated = serde_json::json!({
        "oneOf": [
            {"type": "object", "additionalProperties": false,
             "required": ["verdict", "ask"],
             "properties": {"verdict": {"type": "string", "const": "revise"},
                            "ask": {"type": "string"}}},
            {"type": "object", "additionalProperties": false,
             "required": ["verdict", "citations"],
             "properties": {"verdict": {"type": "string", "const": "approve"},
                            "citations": {"type": "array",
                                          "items": {"type": "string"}}}}
        ]
    });
    assert!(
        compiles(discriminated),
        "a multi-branch oneOf WITH a const discriminator must compile — \
         this is the live verdict-schema shape"
    );

    // Remove every disjointness proof and llguidance refuses `oneOf`
    // outright: "oneOf constraints are not supported. Enable
    // 'coerce_one_of'…" (both `coerce_one_of` and `lenient` are off
    // here and never set).
    //
    // MEASURED 2026-08-19, every case below run before being asserted.
    // A multi-branch `oneOf` compiles only when BOTH hold:
    //   (a) every branch declares `"type": "object"`, AND
    //   (b) the branches are provably disjoint, by EITHER a `const`
    //       discriminator on a shared required key OR
    //       `additionalProperties: false` with disjoint required-key sets.
    // Drop (a) and it fails even with discriminators intact; drop (b)
    // and it fails even with `type` on every branch. Both live
    // production schemas — the tool envelope and
    // `markers.verdict_schema()` — satisfy (a) plus the `const` form
    // of (b), so an edit removing either disarms the constraint.
    let no_proof = serde_json::json!({
        "oneOf": [
            {"type": "object", "required": ["ask"],
             "properties": {"ask": {"type": "string"}}},
            {"type": "object", "required": ["citations"],
             "properties": {"citations": {"type": "array",
                                          "items": {"type": "string"}}}}
        ]
    });
    assert!(
        !compiles(no_proof),
        "a multi-branch oneOf with no disjointness proof must NOT compile \
         — before 2026-08-19 this returned 200 with unconstrained prose \
         instead of an error (markers.py, 2026-08-10)"
    );

    // Proof #2, pinned: sealing the branches makes disjoint required
    // keys sufficient, with no discriminator anywhere.
    let sealed_disjoint_keys = serde_json::json!({
        "oneOf": [
            {"type": "object", "additionalProperties": false,
             "required": ["ask"],
             "properties": {"ask": {"type": "string"}}},
            {"type": "object", "additionalProperties": false,
             "required": ["citations"],
             "properties": {"citations": {"type": "array",
                                          "items": {"type": "string"}}}}
        ]
    });
    assert!(
        compiles(sealed_disjoint_keys),
        "additionalProperties:false + disjoint required keys is the other \
         way to earn the proof"
    );

    // `type` is necessary too, not merely conventional: the same
    // branches with their discriminators intact but no `"type"` are
    // refused. This is the markers.py shape verbatim ("branches declare
    // only properties/required"), and it is why that schema's own
    // linter checks for `"type": "object"` per branch.
    let discriminator_but_no_type = serde_json::json!({
        "oneOf": [
            {"required": ["verdict"],
             "properties": {"verdict": {"type": "string", "const": "revise"}}},
            {"required": ["verdict"],
             "properties": {"verdict": {"type": "string", "const": "approve"}}}
        ]
    });
    assert!(
        !compiles(discriminator_but_no_type),
        "branches must ALSO carry `type: object`; a const discriminator \
         alone is not enough"
    );
}

#[test]
fn additional_properties_walker_does_not_rescue_a_broken_oneof() {
    // `default_additional_properties_false` runs over every schema on
    // the way in. It must not paper over the disjointness failure —
    // if it did, the refusal would depend on argument order.
    let mut broken = serde_json::json!({
        "oneOf": [
            {"required": ["k"], "properties": {"k": {"type": "string", "const": "a"}}},
            {"required": ["k"], "properties": {"k": {"type": "string", "const": "b"}}}
        ]
    });
    default_additional_properties_false(&mut broken);
    assert!(
        !compiles(broken),
        "the walker must not turn an uncompilable oneOf into a compilable \
         one — the refusal has to be a property of the schema, not of the \
         order we process it in"
    );
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

// ─── F3 — the planner's plan schema ────────────────────────────────────
//
// `sovereign_core::planner::plan_schema` is the newest live
// `structured_output` site and the first one built dynamically (the
// `tool_id` enum is the caller's tool list). A schema that fails to
// compile no longer degrades to free-form prose — since F1 it refuses
// the request, so EVERY plan request would 503. These fixtures are the
// gate on that: the shape is proven against the real engine here, not
// discovered on the daemon.

/// Descriptors shaped like the real registry: `plan_schema` embeds each
/// tool's `parameters` verbatim, so a fixture of bare ids would prove
/// nothing about the thing under test.
fn tools(ids: &[&str]) -> Vec<sovereign_core::types::ToolDescriptor> {
    ids.iter()
        .map(|id| {
            tool_with(
                id,
                serde_json::json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": { "query": {"type": "string", "minLength": 1} }
                }),
            )
        })
        .collect()
}

fn tool_with(id: &str, parameters: serde_json::Value) -> sovereign_core::types::ToolDescriptor {
    use sovereign_core::types::*;
    ToolDescriptor {
        id: id.to_string(),
        name: id.to_string(),
        description: id.to_string(),
        parameters,
        examples: vec![],
        effect: Effect::Read,
        idempotency: Idempotency::Idempotent,
        latency: Latency::Fast,
        scope: Scope::Session,
        output_schema: None,
    }
}

fn plan_schema(t: &[sovereign_core::types::ToolDescriptor]) -> serde_json::Value {
    sovereign_core::planner::plan_schema(t).expect("plan_schema must build")
}

/// Drive a byte prefix in and return the matcher, without asserting the
/// prefix is complete — used to park the parser at a decision point.
fn matcher_at(schema: serde_json::Value, prefix: &[u8]) -> Matcher {
    let mut m = matcher_for(schema);
    consume_ok(&mut m, prefix);
    m
}

#[test]
fn plan_schema_compiles_with_and_without_tools() {
    // The whole point of F1 is that this failing is loud. It is also
    // now load-bearing: `LlmPlanner::plan` sets this schema on every
    // request, so an uncompilable one takes planning down entirely
    // rather than quietly widening it.
    assert!(
        compiles(plan_schema(&tools(&["search", "web_search", "sec_facts"]))),
        "the multi-branch plan schema must compile — every branch carries \
         `type: object` plus a `const` kind discriminator (invariant 0479b961)"
    );

    // No tools → the tool/reason_with_tools/delegate branches are
    // dropped rather than carrying `"enum": []`. Two branches remain,
    // so this is still a multi-branch oneOf and still needs the proof.
    assert!(
        compiles(plan_schema(&[])),
        "the no-tools plan schema must compile too"
    );
}

#[test]
fn plan_schema_masks_a_fabricated_tool_id() {
    let schema = plan_schema(&tools(&["search", "sec_facts"]));
    let prefix = br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","tool_id":""#;

    // Twin — the constraint is real only if the legal id is reachable
    // from the same parser state that rejects the illegal one.
    let mut good = matcher_at(schema.clone(), prefix);
    assert!(
        allows(&mut good, b's'),
        "a declared tool id must remain samplable"
    );

    let mut bad = matcher_at(schema, prefix);
    assert!(
        !allows(&mut bad, b'z'),
        "a tool id outside the caller's list must be masked at logit \
         level — the planner cannot name a tool that does not exist"
    );
}

#[test]
fn plan_schema_masks_an_undeclared_step_kind() {
    let schema = plan_schema(&tools(&["search"]));
    let prefix = br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":""#;

    let mut good = matcher_at(schema.clone(), prefix);
    assert!(
        allows(&mut good, b't'),
        "\"tool\" must remain samplable as a kind"
    );

    // `parse_plan_json` used to map any unrecognised kind onto
    // `reason` — a malformed tool step became a silent no-op answer.
    // The grammar now makes the malformed kind unsamplable, and the
    // parser refuses it on the paths the grammar does not cover.
    let mut bad = matcher_at(schema, prefix);
    assert!(
        !allows(&mut bad, b'x'),
        "a step kind outside PLANNABLE_KINDS must be masked"
    );
}

#[test]
fn plan_schema_with_no_tools_masks_the_tool_kind() {
    // A plan cannot call a tool that is not on offer. With an empty
    // tool list the `tool` branch is absent, so "tool" is not a
    // samplable kind at all.
    let schema = plan_schema(&[]);
    let prefix = br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":""#;

    let mut good = matcher_at(schema.clone(), prefix);
    assert!(
        allows(&mut good, b'r'),
        "\"reason\" must stay samplable with no tools available"
    );

    let mut bad = matcher_at(schema, prefix);
    assert!(
        !allows(&mut bad, b't'),
        "\"tool\" must be unsamplable when the caller offered no tools"
    );
}

#[test]
fn plan_schema_params_admits_the_tools_own_keys() {
    // `params` is the NAMED TOOL's own schema, so the walker sealing
    // it with `additionalProperties: false` is correct and wanted: the
    // model may emit the tool's declared keys and nothing else. What
    // must NOT happen is the walker sealing it to the EMPTY object —
    // a tool step that calls a tool with no arguments reads as a real
    // plan and does nothing.
    let mut schema = plan_schema(&tools(&["search"]));
    default_additional_properties_false(&mut schema);
    let prefix = br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","tool_id":"search","params":{"#;
    let mut m = matcher_at(schema, prefix);
    assert!(
        allows(&mut m, b'"'),
        "a tool's params must still admit its declared keys after the \
         additionalProperties walker runs"
    );
}

#[test]
fn plan_schema_key_order_matches_the_prompt_example() {
    // MEASURED 2026-08-19: llguidance emits object keys in the order it
    // ITERATES `properties`, and masks every other order. Not the
    // `required` array order — probed with three permutations of
    // `required` over the same `properties` and the emitted first key
    // never moved. That makes the schema's field order a contract with
    // PLAN_SYSTEM_PROMPT's worked example rather than a style choice —
    // the first draft declared `inputs` before the kind-specific
    // fields and this test caught it, with `"tool_id"` masked directly
    // after `"kind":"tool"`.
    let schema = plan_schema(&tools(&["search"]));

    // The prompt's tool example, verbatim in its own field order.
    let mut m = matcher_for(schema.clone());
    consume_ok(
        &mut m,
        br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","tool_id":"search","params":{"query":"q"},"inputs":[]}],"edges":[]}"#,
    );

    // The same object with `inputs` hoisted ahead of `tool_id` — legal
    // JSON, valid against the schema as a document, and unsamplable.
    let mut m = matcher_for(schema);
    consume_ok(
        &mut m,
        br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","#,
    );
    // The opening quote of the next key, fed separately so the raw
    // string above can end on the comma.
    consume_ok(&mut m, b"\"");
    assert!(
        allows(&mut m, b't'),
        "the declared next key (`tool_id`) must be samplable"
    );
    assert!(
        !allows(&mut m, b'i'),
        "an out-of-order key (`inputs` before `tool_id`) must be masked — \
         if this ever passes, llguidance became order-free and the \
         schema/prompt field-order coupling can be relaxed"
    );
}

#[test]
fn plan_schema_accepts_every_documented_step_kind() {
    // One worked object per branch, driven to the closing brace. A
    // branch that compiles but cannot be driven to completion is a
    // branch the planner can never actually use.
    let schema = plan_schema(&tools(&["search"]));
    let steps: [&[u8]; 5] = [
        br#"{"id":0,"description":"d","kind":"reason","prompt":"p","speed":"slow","inputs":[]}"#,
        br#"{"id":0,"description":"d","kind":"tool","tool_id":"search","params":{"query":"q"},"inputs":[]}"#,
        br#"{"id":0,"description":"d","kind":"reason_with_tools","prompt":"p","speed":"slow","tools":["search"],"max_iterations":6,"inputs":[]}"#,
        br#"{"id":0,"description":"d","kind":"await_user_info","request":{"current_understanding":"u","gap":"g","relevance":"r","satisfying_source":"s","search_hints":["h"]},"inputs":[]}"#,
        br#"{"id":0,"description":"d","kind":"delegate","goal":"g","tools":["search"],"return_schema":{"type":"object"},"max_iterations":6,"inputs":[]}"#,
    ];
    for step in steps {
        let mut m = matcher_for(schema.clone());
        consume_ok(&mut m, br#"{"goal":"g","steps":["#);
        consume_ok(&mut m, step);
        consume_ok(&mut m, br#"],"edges":[[0,1]]}"#);
    }
}

#[test]
fn plan_schema_key_order_depends_on_preserve_order() {
    // The test above proves schema and prompt agree. This one names
    // WHY that agreement is fragile, so a future dependency bump that
    // silently drops `serde_json/preserve_order` reddens here instead
    // of quietly degrading every generated plan.
    //
    // llguidance masks by `properties` ITERATION order. With the
    // feature on, `serde_json::Map` is an IndexMap and iterates in
    // insertion order — which is what `step_branch` controls and what
    // PLAN_SYSTEM_PROMPT's example matches. With it off the map is a
    // BTreeMap, iteration goes alphabetical, and the mask would demand
    // `description, id, inputs, kind, …`. Nothing errors: plans still
    // generate, they are just fought for token by token.
    //
    // Every binary that runs the planner (sovereign-desktop,
    // sovereign-server, sovereign-cli-daemon) resolves the feature
    // transitively today. `sovereign-core` built alone does NOT — so
    // this is a live difference between build graphs, not a
    // hypothetical.
    let probe = serde_json::json!({"zeta": 1, "alpha": 2, "beta": 3});
    let keys: Vec<&str> = probe
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        keys,
        vec!["zeta", "alpha", "beta"],
        "serde_json/preserve_order is OFF in this build graph. The plan \
         schema's key order — and therefore the order llguidance forces \
         the planner to emit — has gone alphabetical and no longer \
         matches PLAN_SYSTEM_PROMPT's worked example."
    );

    // And the schema itself, through the same lens: the first key of a
    // step object must be `id`, as the prompt example shows.
    let schema = plan_schema(&tools(&["search"]));
    let branch = &schema["properties"]["steps"]["items"]["oneOf"][0];
    let first = branch["properties"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    assert_eq!(first, "id", "a step object must open on `id`");
}

#[test]
fn plan_schema_masks_a_non_enum_concept_id() {
    // THE F3 acceptance criterion (plan verification #4, second clause):
    // "a non-enum concept id is unreachable rather than merely
    // discouraged". `sec_facts` publishes a closed `concept` vocabulary
    // built from the compiled concept map; before this, `format_param_hint`
    // rendered it into the prompt and NOTHING masked a non-member token,
    // which is what the sec-facts-concept-enum work was fighting by hand.
    let sec_facts = tool_with(
        "sec_facts",
        serde_json::json!({
            "type": "object",
            "required": ["concept"],
            "properties": {
                "concept": {"type": "string", "enum": ["revenue", "gross_profit"]},
                "period":  {"type": "string"}
            }
        }),
    );
    let schema = plan_schema(&[sec_facts]);
    let prefix = br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","tool_id":"sec_facts","params":{"concept":""#;

    // Twin first — a declared id must stay reachable from this state.
    let mut good = matcher_at(schema.clone(), prefix);
    assert!(allows(&mut good, b'r'), "`revenue` must remain samplable");

    // `ebitda` is the shape the tool rejects and the corpus cannot
    // resolve — an invented id that reads to the operator as a coverage
    // limit rather than a fabrication (note 45b04cf5).
    let mut bad = matcher_at(schema, prefix);
    assert!(
        !allows(&mut bad, b'e'),
        "a concept id outside the tool's declared enum must be UNREACHABLE, \
         not merely discouraged by the prompt"
    );
}

#[test]
fn plan_schema_binds_arguments_to_the_named_tool_not_to_tools_in_general() {
    // Per-tool branches only mean something if the branch actually
    // narrows: `search` takes `query`, `sec_facts` takes `concept`, and
    // neither may borrow the other's arguments.
    let schema = plan_schema(&[
        tool_with(
            "search",
            serde_json::json!({
                "type": "object", "required": ["query"],
                "properties": {"query": {"type": "string", "minLength": 1}}
            }),
        ),
        tool_with(
            "sec_facts",
            serde_json::json!({
                "type": "object", "required": ["concept"],
                "properties": {"concept": {"type": "string", "enum": ["revenue"]}}
            }),
        ),
    ]);

    let open = |id: &str| -> Vec<u8> {
        let mut v =
            br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","tool_id":""#.to_vec();
        v.extend_from_slice(id.as_bytes());
        v.extend_from_slice(br#"","params":{""#);
        v
    };

    let mut m = matcher_at(schema.clone(), &open("search"));
    assert!(allows(&mut m, b'q'), "search must accept `query`");
    let mut m = matcher_at(schema.clone(), &open("search"));
    assert!(!allows(&mut m, b'c'), "search must NOT accept `concept`");

    let mut m = matcher_at(schema.clone(), &open("sec_facts"));
    assert!(allows(&mut m, b'c'), "sec_facts must accept `concept`");
    let mut m = matcher_at(schema, &open("sec_facts"));
    assert!(!allows(&mut m, b'q'), "sec_facts must NOT accept `query`");
}

#[test]
fn plan_schema_compiles_at_registry_scale() {
    // The live registry holds 40 tools, so the real schema is a ~44
    // branch oneOf with a nested argument schema per branch. It is
    // compiled per plan request, so this is both a correctness check
    // and the place a compile-cost regression would show up.
    let ids: Vec<String> = (0..40).map(|i| format!("tool_{i:02}")).collect();
    let descriptors: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            tool_with(
                id,
                serde_json::json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": {"type": "string", "minLength": 1},
                        "mode":  {"type": "string", "enum": [format!("m{i}"), "other"]}
                    }
                }),
            )
        })
        .collect();
    let schema = plan_schema(&descriptors);
    let start = std::time::Instant::now();
    let mut m = matcher_for(schema);
    let elapsed = start.elapsed();
    // Drive one full tool step so the branches are actually exercised.
    consume_ok(
        &mut m,
        br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"tool","tool_id":"tool_07","params":{"query":"q","mode":"m7"},"inputs":[]}],"edges":[]}"#,
    );
    println!("registry-scale grammar compile: {elapsed:?}");
    // No invented latency bar — this asserts the shape compiles and
    // drives, and PRINTS the cost so a regression is visible in the log
    // rather than silently absorbed into plan latency.
}

#[test]
fn plan_schema_bounds_max_iterations() {
    // `max_iterations` was an unbounded integer: a delegate step could
    // declare 1000 and the executor would run them, because it caps
    // nothing — it only ADDS (+2 for Hard, executor.rs:1019). A loop
    // counter a model picks should be unrepresentable above the bound,
    // not merely unlikely.
    let schema = plan_schema(&tools(&["search"]));
    let prefix = br#"{"goal":"g","steps":[{"id":0,"description":"d","kind":"reason_with_tools","prompt":"p","speed":"slow","tools":["search"],"max_iterations":"#;

    // Twin: the documented-typical 6 stays samplable, and drives on.
    let mut good = matcher_at(schema.clone(), prefix);
    consume_ok(&mut good, b"6");
    assert!(
        allows(&mut good, b','),
        "6 — the value PLAN_SYSTEM_PROMPT documents as typical — must be \
         reachable and complete"
    );

    // 999 is masked at the digit that would exceed the ceiling: `9`
    // opens legally (9 <= 12 is undecided at one digit), but a second
    // `9` would put the value past 12 and cannot be sampled.
    let mut bad = matcher_at(schema.clone(), prefix);
    consume_ok(&mut bad, b"9");
    assert!(
        !allows(&mut bad, b'9'),
        "a value past the ceiling must be unreachable, digit by digit"
    );

    // Zero is out too: the executor rewrites it via `.max(1)`, and a
    // value the engine silently rewrites is one the planner should not
    // be able to state.
    let mut zero = matcher_at(schema, prefix);
    assert!(
        !allows(&mut zero, b'0'),
        "0 must be unrepresentable — the executor coerces it to 1"
    );
}
