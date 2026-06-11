# Open Inference Capabilities Protocol (OICP) — v0.3

**Version:** 0.3.0
**Status:** Draft (replaces v0.2 routing vocabulary)
**License:** CC0 (public domain dedication)

---

## Abstract

v0.3 is the current canonical OICP protocol. It introduces
specialization-aware routing via per-request property fields
(`capability_hint`, `latency_class`, `context_tokens`,
`max_output_tokens`) and per-model claim advertisements. The v0.2
capability-profile routing vocabulary (`CapabilityRequirements`,
`satisfies_required`, `score_preferred`, `LatencyPreference`) has
been removed; only the knowledge-search API, response metadata, and
provider-manifest shape survive from v0.2 unchanged.

The internal model-metadata vocabulary (the `Capability` enum,
proficiency levels, capability profiles) is retained inside the
reference implementation (`oicp-types`) as a local type used to
synthesize claims at advertisement time — it is **not** on the wire.

---

## 1. Motivation

v0.2 carries only capability domains (enum) and proficiency levels
(0–4). A mesh that has invested in a code-specialized model cannot
communicate that investment via the protocol: `{code: 4}` is exactly
what a 70B general-purpose model also advertises for code-adjacent
work. Specialization is invisible to the scheduler.

v0.3 introduces `CapabilityClaim` as the unit of scheduling — an
explicit advertisement of which (kind-of-work × latency × context ×
output × affinity) combinations a node serves well. Schedulers rank
`(node, claim)` pairs against a request's properties and route
accordingly.

## 2. Scope — What v0.3 adds (one page)

### 2.1 Capability hint

A capability hint names a kind of inference work. Two hints are
standardized at protocol launch:

- `general` — inference work with no specific specialization target. The default. Every node serving inference supports this hint as a minimum. Every request without a more specific hint uses this.
- `code` — inference work involving code: generation, understanding, modification, review. Models trained or tuned with code emphasis serve this hint with higher affinity than general-purpose models.

Every other specialization (prose, math, biomedical, dialogue, …)
starts as an **extension hint**. Extension hints carry the `x:`
prefix, e.g., `x:prose`, `x:biomed`. The scheduler treats extension
hints identically to standardized hints: matching by string, preferring
higher affinity, falling back to `general` when no node advertises the
requested hint.

### 2.2 Latency class

Three classes, chosen to be unambiguous rough categories:

- `fast` — TTFT in hundreds of milliseconds. Routing, classification, short extractions, interactive UI.
- `normal` — TTFT in single-digit seconds. Most substantive inference. **Default.**
- `extended` — TTFT may be longer; total generation may span tens of seconds or more. Reasoning-heavy work, long-context synthesis, deep planning.

Latency class is distinct from v0.2 `LatencyPreference` (which
expresses a client's desired policy). v0.3 clients may send both;
schedulers should prefer `latency_class` when present and fall back to
the translation helper otherwise.

### 2.3 Capability claim

A provider advertises a `CapabilityClaim` per kind-of-work it handles
well:

```
CapabilityClaim {
    hint:          CapabilityHint,
    latency_class: LatencyClass,
    max_context:   u32,           // tokens
    max_output:    u32,           // tokens
    affinity:      f32,           // [0.0, 1.0]
}
```

A node may publish multiple claims. A node running a single 9B general
model may publish one fast-latency claim (short context, higher
affinity) and one normal-latency claim (longer context, lower affinity
because the model is less ideal for long-form work). A node running
Qwen Coder 32B publishes a `code` claim at `normal` latency with high
affinity.

### 2.4 Inference request properties

An inference request carries four optional property fields:

```
InferenceRequirements {
    … (v0.2 fields) …
    capability_hint:    Option<CapabilityHint>,   // default general
    latency_class:      Option<LatencyClass>,     // default normal
    context_tokens:     Option<u32>,              // actual context
    max_output_tokens:  Option<u32>,              // expected output
}
```

`context_tokens` and `max_output_tokens` are consumed by the scheduler
as **hard feasibility gates**: a claim whose `max_context` or
`max_output` falls short of the request's value is eliminated, not
deprioritized. This reflects that a model which cannot fit the
input or output cannot serve the request regardless of any other
qualities.

### 2.5 Provider model claims

`ProviderModel` gains an optional `claims: Vec<CapabilityClaim>`. Empty
vector means the provider has not yet produced v0.3 claims; consumers
fall back to the v0.2 `capabilities` profile.

## 3. Scheduling

Schedulers SHOULD rank candidate `(node, claim)` pairs using, in
priority order:

1. **Capability hint** — exact match strongly preferred; fallback to `general` always acceptable.
2. **Context capacity** — hard gate (claim eliminated if insufficient).
3. **Output capacity** — hard gate (claim eliminated if insufficient).
4. **Latency class** — soft; adjacent-class mismatch is acceptable but deprioritized.
5. **Affinity** — primary tiebreaker; weighted by observed performance over time (v0.3 §7.4 observation loop).

The detailed scoring function is non-normative and left to the
implementation (see requirements-doc §6 for one worked example). The
protocol fixes only the property semantics and ordering priority.

## 4. Extension governance

Extension hints may be promoted to the standardized set by an
evidence-based governance process:

- The hint must be in measurable use across multiple meshes over a meaningful time window.
- The hint must show measurable routing benefit over `general` fallback.
- The semantics of the hint must be stable — different nodes advertising the same hint must be doing approximately the same thing.

Promotion is conservative by design. Implementations MUST NOT promote
hints unilaterally; only the governance body may add entries to the
standardized set.

## 5. Backward compatibility

- A v0.3 manifest containing empty `claims` is functionally identical to a v0.2 manifest.
- A v0.2 client sending no property fields is treated as `{capability_hint: general, latency_class: normal}` by a v0.3 scheduler.
- A v0.3 client sending property fields to a v0.2 scheduler gets its v0.2 `capabilities`/`performance`/`context` requirements honoured; the v0.3 fields are simply ignored.
- Bare unknown hints (not currently standardized, no `x:` prefix) are preserved verbatim so future standardization cycles don't break older parsers.

## 6. Translation helpers (non-normative)

The reference implementation in `oicp-types` ships two helpers that
schedulers can use to synthesize v0.3 properties when only a v0.2
counterparty is available:

- `infer_hint_from_profile(CapabilityProfile) → CapabilityHint` — returns `code` when code proficiency ≥ 3 (Strong), else `general`. Intentionally conservative: "handles code adequately" is not a specialization.
- `latency_class_from_preference(LatencyPreference) → LatencyClass` — `Interactive → Fast`, `Throughput | Background → Extended`, `BestEffort → Normal`.

## 6a. The composed scorer (reference implementation, normative for this codebase)

As of 2026-06-10 the operational scoring product lives ONCE, in
`oicp-types` (`score_with_adjustments` → `ScoreBreakdown`), consumed
by every scheduler in the workspace (sovereign-mesh peer selection,
sovereign-inference backend selection). The product:

```
final_score = claim_score                     # §3/§5 hint × latency × affinity
            × observation_mult                # effective_affinity(claimed, obs) / claimed
            × load_penalty                    # 1 / (1 + 0.05·in_flight)
            × locality_bonus                  # Local 1.15 / Near 1.05 / Far 1.0
            × cold_start_weight               # 0.7 → 1.0 over 20 samples
            × throughput_factor               # [0.3, 1.0], observed > benchmark > neutral
            × availability                    # gossiped inference_availability, clamp [0.2, 1.0]
```

The `availability` term is **normative for this codebase** (adopted
2026-06-10): a peer gossiping low `inference_availability` is
deprioritized everywhere, floored at 0.2 so a busy peer stays
routable. Callers with no signal pass `None` (= 1.0) — notably a
node scoring ITSELF, whose business is already captured by
`in_flight`. Every consumer logs the full `ScoreBreakdown` per
candidate, which is the glassbox contract: any routing decision is
reconstructible from one trace event.

## 7. What v0.3 does not change

- The v0.2 `Capability` enum, `CapabilityProfile`, and the `required`/`preferred` scoring helpers remain unchanged and continue to be the fallback routing path.
- The `ProviderManifest` top-level shape (provider, models, knowledge, federation) is unchanged.
- The knowledge-search API (§6 of v0.2) is unchanged.
- The privacy model (`ShardingPrivacy`, default `LocalOnly`) is unchanged.

---

See the implementation in `oicp-types/src/lib.rs` and the integration
roadmap in `sovereign/SYSTEM_OVERVIEW.md` §4.8 and §12.
