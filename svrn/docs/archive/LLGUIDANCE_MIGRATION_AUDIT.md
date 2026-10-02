# Llguidance Migration Audit

**Status:** **SHIPPED 2026-05-22.** D-full landed —
`json_constraint.rs` deleted (-5623 LOC), `vocab_bytes_for` +
`non_latin_denylist_for` extracted to `vocab_cache.rs`, env gate
removed, `ConstrainedSampler::constraint` field retired.
`structured_output` and `lark_grammar` both route through
`LlguidanceConstraint`. All sovereign-inference tests green.

**Engines under audit:**
- `sovereign-inference::json_constraint::JsonConstraint` (5623 LOC,
  in-house JSON-Schema FSM + Tier 1/2 jump-forward).
- `sovereign-inference::llguidance_constraint::LlguidanceConstraint`
  (543 LOC, adapter over `llguidance` 1.7).

**Predecessors:**
- `memory/project_llguidance_readoption_plan.md` — D-lite (parallel
  engine, lark path only).
- `SYSTEM_OVERVIEW.md §10.1` — `json_constraint.rs` is listed as a
  split-deferral; this audit is the retire-not-split move.

---

## §1 — Feature surface comparison

### §1.1 JsonConstraint supported features

From `compile_schema` (json_constraint.rs:190) + `Schema` enum
(json_constraint.rs:63):

| Feature | Status |
|---|---|
| `type: object` (properties, required) | ✓ |
| `type: array` + `items` + `maxItems` | ✓ |
| `type: string` (free-form, `enum`, `maxLength`) | ✓ |
| `type: integer` / `number` / `boolean` / `null` | ✓ (typed match only) |
| `type: ["string", "null"]` (type union) | ✓ (expands to anyOf) |
| `anyOf`, `oneOf` | ✓ |
| `$ref` to `#/$defs/` or `#/definitions/` | ✓ (local only) |
| `additionalProperties: true/false` | ✓ — **DEFAULT IS FALSE** (non-spec) |
| `pattern` | ⚠ literal-prefix subset only (`^<literal>`); rejects regex metas |
| `x-asciiExtended` (custom) | ✓ — 2-byte UTF-8 cap on string body |

### §1.2 JsonConstraint **rejected** features (compile error)

`format`, `not`, `allOf`, `if/then/else`, external `$ref`, rich
`pattern` regex, numeric `minimum`/`maximum`, `minLength`,
`minItems`.

### §1.3 JsonConstraint **silently ignored** features

The compiler walks the JSON object and matches known keys. Unknown
keys produce no error. This means:

- `minimum` / `maximum` on `integer` (used in `score.rs:780`).
- `minItems` on array (used in `runtime.rs:4148`).

These constraints are present in 2 of the 11 schemas but **never
enforced**. Migration would change behaviour from "silently
permissive" → "actually enforced" — likely a net positive, but
needs verification per site (see §3.B).

**Correction (2026-05-22):** an earlier draft of this section
listed `maxLength` as silently ignored. That was wrong. JsonConstraint
fully enforces `maxLength` per its `Schema::StringAny { max_length, … }`
field (`json_constraint.rs:84-98`), counted in unicode code points
(UTF-8 start bytes). Both engines treat the field identically for
the schemas in §2 (all rationale strings are English ASCII). It is
NOT a candidate explanation for any observed bench drift.

### §1.4 Llguidance feature surface (1.7 / `from_json_schema`)

`TopLevelGrammar::from_json_schema` accepts the JSON-Schema 2020-12
subset that llguidance supports natively (per crate docs +
`from_json_schema_rejects_incomplete_envelope` proof in
`llguidance_constraint.rs:421`). The 2026-05-21 work pinned this
path as **strict-closure** — it rejects incomplete envelopes byte by
byte, which is exactly what we want.

Llguidance's covered surface is **strict superset** of what
`JsonConstraint::compile_schema` accepts, including:

- All of §1.1 except `x-asciiExtended` (custom keyword, ignored by
  llguidance — see §3.E).
- All of §1.2 (numeric bounds, length bounds, full-regex `pattern`).
- Full JSON Schema Draft 2020-12 compositions (`if/then/else`,
  `not`, `allOf` — though we don't use these today).
- `pattern` accepts ECMA regex via llguidance's bundled
  `regex_syntax` (full alternation, classes, quantifiers).
- `const` (no caller uses this today; we use `enum: ["x"]` as the
  workaround per `inference_adapter.rs:870`).

**Default `additionalProperties` follows spec (TRUE).** Bridge layer
must inject `additionalProperties: false` to preserve current
behaviour — see §3.A.

---

## §2 — Schema sites inventory

Eleven static `structured_output: Some(schema)` sites + one dynamic
tool-envelope generator. Per-site verdict on llguidance migration.

| # | File:line | Purpose | Features used | Verdict |
|---|---|---|---|---|
| 1 | `runtime.rs:4162` | Title-expansion: `{"titles": [str, …]}` | object, array, string, **minItems** (silent), maxItems | drop-in; bridge adds `additionalProperties:false` |
| 2 | `runtime.rs:11842` | Map/reduce prompt synth | object, string | drop-in |
| 3 | `context.rs:165` | History summarisation | object, string | drop-in |
| 4 | `context.rs:331` | Topic / domain extractor | object, string | drop-in |
| 5 | `gap.rs:134` | Research gap classifier | object, boolean, string, array, partially required | drop-in |
| 6 | `router.rs:1438` | Intent classifier (9-enum) | object, string + enum | drop-in |
| 7 | `router.rs:1485` | Tool selector (dynamic enum) | object, string + enum | drop-in |
| 8 | `score.rs:271` | Concept-present judge | object, string + enum(yes/no), string | drop-in |
| 9 | `score.rs:490` | Loose-credit judge | object, array (maxItems), string (maxLength) | drop-in |
| 10 | `score.rs:815` | Essay-readiness rubric | object, **integer + min/max** (silent), string maxLength | behavioural change — see §3.B |
| 11 | `runner_threads.rs:470` | Per-fact thread judge | nested object/array, type union `["integer","null"]`, string enum | drop-in; type-union path needs fixture |
| dyn | `inference_adapter.rs:839` | Tool envelope (`oneOf` over per-tool objects, additionalProperties:false explicit, `pattern` cmd_prefix when set) | oneOf, object, string-enum, opaque arguments schema, full pattern when cmd_prefix | already on llguidance path; see §3.D |

**Observation:** every static schema is small (≤ 6 properties), no
schema uses `$ref` / `$defs`, none use `anyOf` directly (only oneOf
in tool envelope). None use `format`, `if/then/else`, `not`,
`allOf`. Migration surface is narrow.

---

## §3 — Risk register

### §3.A `additionalProperties` default flip — **HIGH**

JsonConstraint defaults to `false` (non-spec). Llguidance follows
spec (defaults `true`). Of the 11 static sites, **none** set
`additionalProperties` explicitly. Under llguidance, the model could
emit trailing fields the JsonConstraint mask used to forbid.

**Mitigation options:**
1. **Bridge wrap** (recommended): teach the schema→grammar bridge to
   walk the schema once and inject `additionalProperties: false` on
   every typed object that doesn't set it. One ~30-line walker, no
   per-site edits.
2. Per-site explicit `additionalProperties: false` — 11 edits + risk
   of new schemas forgetting.

**Fixture:** unit test compiling schema #1 (titles) against
llguidance + feeding model output `{"titles":["A"],"extra":1}` —
must be masked.

### §3.B Silent bounds suddenly enforced — **MEDIUM**

Two sites have constraints JsonConstraint silently ignored:
- `runtime.rs:4148` — `minItems: 1` on titles array.
- `score.rs:780-783` — `minimum: 0, maximum: 3` on four integer
  rubric scores.

Under llguidance these become real. Model emitting `"titles": []`
or `"argument_depth": 4` would be masked. **Likely an improvement**
(current behaviour is "schema says X, sampler ignores X"). But it
shifts where the failure shows up:
- Old behaviour: model produced `argument_depth: 5`, downstream
  parser accepted it, rubric noise undetected.
- New behaviour: model masked from producing `5`, must produce
  `0-3`. May iterate or stall if it strongly preferred a bad
  number.

**Fixture:** force the model to produce `argument_depth: 5` against
the old + new path. Compare warm tok/s and final value. If
llguidance pushes the rubric score into a worse number (e.g. `0`
instead of `5`), the rubric becomes less informative — flag the
caller for schema rework.

### §4.2 first datapoint — `bench all --synth sep` 2026-05-22

**Initial run vs 7-day-old baseline** appeared to show drift
(contested −16, position_summary −7, +18 comparative etc).
Diagnosis: baseline was captured 2026-05-15, predating llguidance,
MTP fast-path, and ~5 days of other daemon work. Apples-to-oranges.

**Apples-to-apples test (gate=OFF vs gate=ON, both runs today):**

| Category | JSON-side today | LLG-side today | Δ |
|---|---|---|---|
| argument_reconstruction | 35/38 = 0.92 | 35/38 = 0.92 | 0 |
| comparative | 30/31 = 0.97 | 30/31 = 0.97 | 0 |
| concept_distinction | 12/14 = 0.86 | 12/14 = 0.86 | 0 |
| contested | 21/25 = 0.84 | 21/25 = 0.84 | 0 |
| dialectical | 17/21 = 0.81 | 17/21 = 0.81 | 0 |
| position_summary | 28/30 = 0.93 | 28/30 = 0.93 | 0 |
| **OVERALL** | **143/159 = 0.90** | **143/159 = 0.90** | **0** |

**Byte-identical synth-answer texts across all 21 questions.**
Zero judge-verdict flips. The two engines produce structurally
equivalent outputs.

**Conclusion:** llguidance is a drop-in replacement for
JsonConstraint on the full sep-synth pipeline (router + topic
extractor + answer judge + loose-credit + essay-readiness +
tool envelope). The migration is shippable. Action items:

1. Refresh the sep baseline against current daemon state (so
   future bench-all diffs use today's snapshot, not 2026-05-15).
2. Run `bench all --synth` across remaining banks (wikipedia,
   atlas, conversation) for broader signal. Same pattern
   expected.
3. Flip `SOVEREIGN_FULL_LLGUIDANCE` default to on, or remove the
   gate entirely.
4. Stage the `json_constraint.rs` deletion (~5623 LOC).

### §3.C/§3.G first datapoint — `bench_constraint` smoke 2026-05-22

Initial smoke run on Apple Silicon Metal (Qwen3.5-2B, 2 iters,
60-token cap, single ctx) revealed the perf shape is **the
opposite of what we feared**:

```
          engine    iters   decode tok/s  mask p50 us  mask p99 us     ff_yield
 json_constraint        2           37.5         9080        68649            —
      llguidance        2           94.2         7786        34031  0.00 (26/26 empty)
```

Three signals worth pinning before doing the larger sweep:

1. **llguidance is 2.5× faster on decode tok/s** even WITHOUT
   jump-forward wired into the sampler hot path. The audit §3.G
   hypothesis (llguidance ≥ JsonConstraint baseline) understated
   the win: it's a meaningful speedup, not a wash.

2. **mask p99 ~half** (34ms vs 69ms). The JsonConstraint full-vocab
   per-candidate parser walk (`embedded.rs:7821`) is exactly the
   pathology llguidance's precomputed bitmask retires.

3. **ff_yield = 0.00** across 26 sample points. This is the audit
   §6 #1 question answered empirically: `ApproximateTokEnv` returns
   empty `compute_ff_tokens` on Qwen 3.5 BPE every time. Tier 2
   jump-forward equivalent **doesn't fire** at all on this path
   without the custom `TokenizerEnv` (re-adoption plan Q1 path B).

What this changes:

- §3.C is no longer a blocker. llguidance wins decode tok/s on
  mask alone, before we get the jump-forward back.
- Custom `TokenizerEnv` (Q1 path B) becomes a follow-up
  optimisation, not a prerequisite. The headline number already
  clears the migration bar.
- The audit §5 acceptance threshold of "≥ 0.85× json baseline" is
  trivially met on this slot. Run the sweep on the 9B + A3B slots
  to confirm before flipping default.

Caveat: this is a 2-iter smoke with a 2B model and a tiny gen
budget. Run `bench_constraint --iters 20 --gen-tokens 400` against
Qwen3.5-9B + Qwen3.6-35B-A3B before the operator A/B kicks off.
And confirm the same shape under `bench all --synth` — that's
the real regression gate per §4.2.

### §3.C Jump-forward perf regression — **MEDIUM/HIGH**

`embedded.rs:1647` notes the Tier 1 + Tier 2 jump-forward gives
~2-3× on Strix Halo Vulkan for schema-heavy paths (title-expansion,
intent classifier, every Phase 1 atlas extraction).

- **Tier 2 (`forced_next_run`)** — analog is
  `Matcher::compute_ff_tokens()`, already exposed via
  `LlguidanceConstraint::forced_ff_tokens()`. Needs wiring in
  `ConstrainedSampler::forced_next_run` enum-dispatch.
- **Tier 1 (`forced_next_token`)** — no exact analog; emulate as
  `forced_ff_tokens().first()`. Verify it returns a single token in
  cases where JsonConstraint did (deterministic open-quote,
  separator commas, closing braces).
- **`ApproximateTokEnv`** — per llguidance docs, returns empty
  `compute_ff_tokens` when tokenisation isn't canonical (token byte
  rendering doesn't match the model's true tokenizer). For Qwen,
  Gemma, BPE variants this likely produces empty runs frequently.

**Mitigation if `ApproximateTokEnv` ff is too empty:** implement a
custom `TokenizerEnv` over `LlamaModel` (Q1 path B in the
re-adoption plan). +3 day cost, but reusable across all engines.

**Fixture:** repeat warm tok/s benchmark, schema-heavy workload
(title-expansion × 50 calls), Strix Halo Vulkan slot. Acceptance
threshold: ≤ 15% regression. If worse, build path B before
flipping.

### §3.D Tool envelope `pattern` with cmd_prefix — **LOW**

`tool_envelope_schema_for_with_env_and_cmd_prefix`
(`inference_adapter.rs:687`) injects `pattern: "^<literal>"` on the
`cmd` field when the request carries `cmd_prefix`. JsonConstraint
parses this via `parse_literal_prefix_pattern` (line 145), rejecting
regex metas.

Llguidance accepts full regex via `regex_syntax`. The literal-prefix
form `^abc` is a valid regex too — should compile cleanly. **No
action needed**, but pin a fixture (cmd_prefix = `cargo `) to verify
the model is forced to emit the prefix byte-by-byte under
llguidance (same UX as JsonConstraint today).

### §3.E `x-asciiExtended` keyword — **LOW (orthogonal)**

Custom keyword on `StringAny`. Llguidance ignores it (unknown
keyword per JSON Schema spec). **Already covered by a parallel
mechanism:** `non_latin_denylist_for(model)` is a vocab-level bitmap
applied in `ConstrainedSampler::sample()` independent of any JSON
constraint, gated on `SOVEREIGN_BLOCK_NON_LATIN=1`.

**Conclusion:** dropping the schema-level keyword loses nothing as
long as the env-gated denylist path stays. Schema-level granularity
(blocking CJK in just one string field while allowing it elsewhere)
is currently **unused** — grep shows zero callers set
`x-asciiExtended`. Safe to drop.

### §3.F Compile-error mismatch — **LOW**

JsonConstraint returns `ConstraintError::Unsupported{feature, pointer}`
with a specific feature name. Llguidance's compile error format
differs. Both engines fall back to free-form sampling on compile
failure (per `embedded.rs:8013`, `embedded.rs:8037`), so user-facing
impact is the warn-log text only.

**Action:** smoke-test that the operator-runbook still understands
the new warn texture. Minor docs update.

### §3.G Mask compute perf — **LOW (likely improvement)**

JsonConstraint mask is O(152K × per-candidate parser cost) per
`embedded.rs:7821`. Llguidance returns a bitmask directly from
`compute_mask`. Almost certainly **faster** on the hot path, but
unverified.

**Fixture:** structured-output throughput on a Qwen3.5-9B prompt.
Acceptance: no regression. Hypothesis: 1.2-2× speedup.

---

## §4 — Fixture plan

Two layers: unit-test parity fixtures (live in
`sovereign-inference/tests/llguidance_parity.rs`) + end-to-end
regression via the existing `sovereign bench all` harness.

### §4.1 Unit-test fixtures

| Fixture | Validates |
|---|---|
| `parity_titles_array` | §3.A defaults to additionalProperties:false bridge; §3.B minItems suddenly enforced |
| `parity_essay_readiness_integer_bounds` | §3.B minimum/maximum enforced; downstream rubric still useful |
| `parity_thread_judge_type_union` | type:["integer","null"] expands correctly |
| `parity_intent_enum_router` | §1.1 enum drop-in |
| `parity_tool_envelope_oneof_with_cmd_prefix` | §3.D pattern handling |
| `unit_default_additional_properties_walker` | §3.A bridge implementation correctness |

### §4.2 End-to-end regression — `sovereign bench all --synth`

The canonical migration regression gate. The bench harness already
drives schema-constrained call sites liberally:

- **Router intent classifier** (§2 row #6) — every bench question
  hits this on the routing pass.
- **Topic / domain extractor** (§2 row #4) — wikipedia bench
  multi-turn cases.
- **Loose-credit + concept + essay-readiness judges** (§2 rows
  #8/#9/#10) — every retrieval-judge surface (sep, atlas,
  wikipedia) runs these per question.
- **Per-fact thread judge** (§2 row #11) — conversation bench.
- **Tool envelope schema** (§2 dynamic) — agent-coding bench's
  bash/edit/read tool calls.

Workflow:

```sh
# Baseline (default — JsonConstraint path).
sovereign bench all --synth --report /tmp/bench-json.json

# Restart daemon with the gate on, then re-run.
SOVEREIGN_FULL_LLGUIDANCE=1 sovereign daemon restart
sovereign bench all --synth --report /tmp/bench-llg.json

# Compare. A clean migration shows no bench regressed past the
# threshold (default 0.5pt of F1).
```

**Acceptance:** zero `regressed` cells across all discovered
benches. `improved` cells are fine (§3.B silent bounds suddenly
enforced should produce some). `first_run` cells on fresh bench
banks are also fine.

If both runs need to coexist in CI, add an `-llg` suffix to the
bench id so the two baselines don't overwrite each other (mirror
the existing `-synth`/`-routing` suffix shape in
`bench_cmd::all::baseline_bench`). One follow-up PR if we want
automated A/B in CI.

### §4.3 Microbench — `examples/bench_constraint.rs`

Single-model A/B harness measuring decode tok/s, mask p50/p99
latency, and llguidance ff-token yield. Subordinate to
`bench all` (which measures correctness across the full pipeline);
use the microbench when you need to attribute a regression to the
mask path specifically vs. the router or judge.

```sh
cargo run --release -p sovereign-inference --example bench_constraint -- \
    --model ~/.svrnmesh/models/Qwen3.5-9B.Q8_0.1.gguf \
    --engine both --iters 5 --gen-tokens 200
```

Reports per-engine warm tok/s + mask p50/p99 + llguidance ff_yield
(empty-rate answers audit §6 #1 — whether `ApproximateTokEnv` is
viable without the custom `TokenizerEnv` path B).

**Sequencing:** §3.A walker + parity fixtures land first (done).
Bridge wiring + env gate land second (done). `bench all --synth`
A/B is the rollout gate. Microbench is the diagnostic for
attributing perf signals when something does regress.

---

## §5 — Audit verdict

**D-full migration is viable** under three conditions:

1. **Bridge layer adds `additionalProperties: false` default** for
   any object schema that doesn't set it (§3.A). One walker, lives
   in the schema→llguidance entry point.

2. **`forced_ff_tokens` perf parity** verified on Strix Halo Vulkan
   (§3.C). If `ApproximateTokEnv` empties too often, build custom
   `TokenizerEnv` first (Q1 path B from re-adoption plan).

3. **Two schemas accept silent→enforced bounds** (§3.B, schemas #1
   and #10). Likely an improvement, but explicitly acknowledge the
   behaviour change.

If all three hold, the deletion math is favourable:
- **-5623 LOC** (`json_constraint.rs`)
- **-1 deferred-split entry** in SYSTEM_OVERVIEW §10.1
- **+1 dependency** (`llguidance` 1.7, already in tree)
- **+1 small `additionalProperties` walker** in the bridge
- **0 net behavioural regressions** with stricter enforcement on
  silently-ignored bounds.

**Recommended next move:** land the fixture set + walker
(`feat(inference): llguidance parity fixtures + additional_properties walker`)
as a self-contained PR. Defer the wiring PR until those go green +
the perf bench is clean.

---

## §6 — Open questions for next session

1. Does `ApproximateTokEnv::compute_ff_tokens` empty out for Qwen
   3.5 / Gemma 4 BPE tokenizers? Empirical question — run the
   bench once and find out.
2. Should the `additionalProperties:false` walker live in the
   bridge (`inference_adapter::build_completion_request`) or in
   `LlguidanceConstraint::new`? Argument for the bridge: keeps the
   non-spec default explicit at the schema-construction boundary.
   Argument for the constraint: applies uniformly to every caller,
   including future ones.
3. Is `non_latin_denylist_for` still load-bearing? Grep shows zero
   `SOVEREIGN_BLOCK_NON_LATIN` documentation hits. Verify with the
   operator before retiring it alongside `x-asciiExtended`.
