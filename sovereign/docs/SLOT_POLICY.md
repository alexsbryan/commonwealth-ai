# Slot Policy — declaring inference needs, and how the mesh routes them

Status: **draft for adoption, 2026-07-08 (rev 2 — OICP-first).**
Once adopted, this is the normative policy for how inference work
finds a model. Every new call site must be justifiable against this
document; a reviewer should be able to point at the workload class it
belongs to and the requirement bundle that class implies.

The one-sentence version: **call sites declare what the job needs;
the scheduler finds the most suitable slot in the cluster.** A call
site never picks a slot. "Fast vs primary" is not a client decision —
it is the *outcome* of OICP claim scoring, and the local node is just
the degenerate one-node mesh.

Companion docs: `inference.md` (slot mechanics, scoring pipeline),
`commonwealth/docs/oicp-v0.4.md` (the wire contract: requirements,
claims, features, constraint negotiation),
`commonwealth/docs/routing-field-guide.md` (where a request
physically goes today).

---

## 1. The scheduling model

The unit of scheduling is the **mesh**, not the node. Every node, by
construction, runs the same slot shape — a small always-resident Fast
model and a hardware-scaled Primary — and advertises each slot as
OICP **claims** (`oicp_synthesis::synthesize_slot_claims`):

| Advertised claim | From slot | latency_class | hard gates (ctx / out) | notes |
|---|---|---|---|---|
| FastShort | Fast | `fast` | 2 048 / 512 | +0.05 affinity — wins when both fast claims pass |
| FastLong | Fast | `fast` | 8 000 / 24 576 | |
| Normal | Primary | `normal` | 32 768 / 4 000 | also aliased `primary`, `commonwealth/primary` |
| Code specialist | Code (lazy swap) | `normal` | 32 768 / 4 000 | `code` hint, `loaded: false` |
| Extras | operator slots | `normal` | per model | |

A request carries `InferenceRequirements` — capability hint, latency
class, context tokens, max output tokens, privacy — and (v0.4) may
depend on host **features** (`constraint:json_schema`,
`constraint:lark`, `think_budget`, `x:*` extensions). The three
schedulers (`commonwealth-inference`, `sovereign-inference::selector`,
`sovereign-mesh::oicp_select`) all resolve requirements against
claims through the one spec scoring function, then fold in
operational signals (load, locality, health, throughput, cold-start).
**Local wins ties.**

What this buys, and why the policy exists: a judge call declared
honestly (`latency: normal`, schema constraint) can land on the
beefiest idle primary in the cluster instead of being hard-pinned to
whatever the local box loaded. A routing classify declared honestly
(`latency: fast`, `max_output ≤ 512`) hard-gates onto a FastShort
claim anywhere — in practice locally, because latency-fast work never
crosses the network (§5). The scheduler can only be this smart if
call sites tell the truth; the workload classes below are the
truth-telling vocabulary.

The reference implementation of the pattern already exists:
`enrich_cmd/inference_client.rs` attaches
`{oicp_version, max_output_tokens, latency_class: fast}` and lets the
hard gates select FastShort vs FastLong. That — not a `Speed`
literal — is what every call site converges to.

## 2. The request contract

A call site expresses exactly four things, and nothing else:

1. **Capability hint** — `general`, `code`, or an `x:` extension.
   One claim per kind-of-work a node serves well; specific-vs-general
   scoring is the spec's job, not yours.
2. **Latency class** — `fast` (interactive reflex, sub-second TTFT),
   `normal` (most work), `extended` (deep reasoning; only ever by
   explicit intent/skill declaration, never derived).
3. **Honest budgets** — `context_tokens` and `max_output_tokens` as
   the job actually needs. These are hard gates: they ARE the
   FastShort/FastLong/Normal selector. Inflating `max_output` "to be
   safe" silently disqualifies the batched claim; understating it
   truncates. Budgets are part of the routing contract.
4. **Privacy** — `LocalOnly` (default) or `MeshAllowed`, inherited
   from the session/operator sharing posture, never hardcoded per
   call site.

Plus, when the output must be machine-consumed: a **constraint**
(JSON schema or lark grammar) and `think_budget: 0` — negotiated via
v0.4 features, with the §3.1 degrade ladder as fallback semantics.

What a call site must NOT do:

- **Never set `preferred_speed`.** It is a serving-side internal,
  derived from the latency class in one canonical module during the
  transition (§8) and slated to disappear from the request path.
- **Never pin `model_id`** except to honor a *user's* explicit model
  choice (the "honour the name" contract). Pinning `"primary"` to
  smuggle a quality requirement bypasses the scheduler, breaks
  BYOM/shared-model/pool configs, and hides the real requirement.
- **Never send a bare request with no envelope.** The no-envelope
  path is legacy; during transition it resolves to Primary
  (quality-by-default, §8).

## 3. Workload classes → requirement bundles

Every call site belongs to exactly one class. The class — not the
call site — owns the requirement bundle.

| Class | hint | latency | max_output | constraint / features | Definition & examples |
|---|---|---|---|---|---|
| **Route** | general | fast | ≤ 256 | json_schema, think 0 | Classification consumed by control flow, never shown to the user: router passes, intent/tool select, doc-type detect, difficulty estimate, branch conditions |
| **Housekeep** | general | fast | ≤ 512 | schema where structured, think 0 | Turn-loop hygiene producing advisory context, not durable truth: titles, preambles, topic extraction, working-memory compression, note digests, gap checks, re-query generation |
| **ExtractDurable** | general | normal | as needed | json_schema or lark | Extraction written to a durable store or protecting one: memory facts, contradiction/tension checks, skeleton entities/segments/motifs, typed extension, RAPTOR summaries. Corruption outlives the session; tiny budget ≠ tiny stakes |
| **EnrichBulk** | general | fast | ≤ 512 where possible | grammar REQUIRED | High-volume corpus enrichment where fast-class throughput is existential and quality is bench-validated per-recipe. Mesh-native: fans across every node's fast claims |
| **Judge** | general (+ `x:forced_choice` for logprob elicitation) | normal | small | json_schema; forced-choice feature | Anything that scores, ranks, verifies, or gates another output: grounding critics, sufficiency judges, eval judges, voice judge, best-of-N selection. Cluster-routable to the best available primary — this replaces today's local `model_id:"primary"` pins |
| **Synthesize** | general / code per intent | normal (extended only by intent/skill declaration) | response-length setting | — | Prose composed for the user; final reduces of document ops; pipeline Drafter/Presenter |
| **Passthrough** | per user | normal+ | user setting | tools require a tools-capable template ⇒ never fast claims | Naked chat, planner, executor reasoning steps, delegate workers — the model the user chose, doing what they ran it for |

The chat-turn intent table
(`intent_helpers.rs::default_oicp_for_intent`) remains the
authoritative per-intent hint/latency source, with skill overrides;
it is the Synthesize/Passthrough bundle generator. The classes above
govern everything the intent table doesn't reach — the sixty-odd
internal calls the audit found free-handing `Speed` literals.

## 4. Hard rules

1. **Requirements are the contract; slots are the scheduler's
   answer.** No call site names a slot, a speed, or (outside user
   choice) a model.
2. **Judging, logprob elicitation, durable writes, and tool-bearing
   calls declare `latency: normal` or higher** — they must never
   land on fast-class claims. (Fast-slot models cannot express tool
   calls and produce untrustworthy logprobs — documented failures.)
3. **User-visible prose declares `normal`+**, except §6.1.
4. **`extended` is never derived** — only intent/skill declarations
   produce it.
5. **Budgets are honest.** Fast-class calls size `max_output ≤ 512`
   unless the output genuinely needs more; a fast-class call above
   512 forfeits the batched claim and must say why in a comment.
6. **Machine-consumed output carries a constraint** (schema/grammar)
   and negotiates it via v0.4 features — never "small model plus
   hope."
7. **Quality by default.** Absent requirements resolve to Primary.
8. **Privacy is a session/operator posture,** not a per-call-site
   decision.

## 5. Scheduler policy (normative local decisions on top of the spec)

The spec scores; these are the policy choices our schedulers layer on:

- **Latency-fast work never crosses the network.** Network RTT
  violates the class's promise, so `latency_class: fast` requests are
  served from local claims only. This is the principled successor to
  the old `Speed::Slow`-only mesh gate — eligibility is now
  `latency_class != fast AND privacy == MeshAllowed`, then scoring.
  (EnrichBulk is the deliberate exception: batch fan-out MAY submit
  fast-class work to peers explicitly, because no interactive user is
  waiting on any single call.)
- **Local wins ties** — never cross the network for free
  (`pick_better`: score, then smaller size, then incumbent).
- **Hint veto stands** (`pick_slot_v03`): a latency-chosen slot that
  cannot serve the requested specialization yields to the other slot.
- **Feature-gated placement:** a request requiring
  `x:forced_choice` or a constraint feature is only eligible for
  claims/hosts advertising it. Primary slots advertise
  `x:forced_choice`; fast slots do not. This — not a model pin — is
  how judge-grade work finds a trustworthy model anywhere in the
  cluster.
- **Serving side keeps a shape backstop:** `pick_slot` routes any
  request carrying forced-choice or tools away from Fast regardless
  of what the envelope claims, mirroring the existing tools guard.

## 6. Sanctioned exceptions — each owns its risk

**6.1 FastFocused synthesis** (`resolve_synthesis_route`). When
evidence is decisively concentrated or the intent is a bounded
comparison, synthesis MAY declare `latency: fast`. Risk owner: the
grounding gate. Revisit trigger: any recurrence of untagged-CoT or
fabrication incidents on this path (the fast-slot KQ CoT leak of
2026-06-10 is the cautionary precedent).

**6.2 EnrichBulk on fast-class claims.** Throughput is existential;
quality is bench-validated per recipe; grammar constraint is
mandatory. Revisit trigger: grammar-constrained primary throughput
reaching ~10 s/chunk on `default`-tier hardware.

**6.3 KQ empty-retrieval fallback** (`knowledge_query.rs:407`) —
**provisional, under review.** User-visible prose on a fast claim
with the CoT-leak history; 300-token cap is the blast-radius bound
until re-adjudication (rationalization plan P5e).

## 7. Glassbox requirement

Every request carries its workload class into tracing (`workload=`)
alongside the existing `slot_picker` telemetry, and the scheduler's
choice is explainable from the log line: which claims were scored,
which gates disqualified, why the winner won. "Which model served
this and why" must be answerable without a debugger — on any node.

## 8. Transitional reality — `Speed` and the legacy paths

Until the rationalization plan completes:

- `preferred_speed` still exists on `CompletionRequest`. It is
  derived from the latency class in ONE canonical module
  (`Fast↔fast`, `Slow↔normal`, `extended→Slow` serve-side) and never
  written by call sites migrated to the workload resolver.
- **`Speed::Medium` is deprecated** — an alias of `Slow` everywhere
  physical; its single accidental divergence (the old mesh gate
  reading `== Slow`) is retired by §5. Serde compatibility retained.
- The derive `#[default] Fast` on `Speed` violates rule 4.7 and is
  slated for removal; `CompletionRequest::new`'s `Medium` aligns to
  the same default.
- No-envelope requests resolve to Primary
  (`pick_slot_for_oicp → Speed::Slow`) — already policy-conformant;
  the path itself is scheduled for elimination as call sites migrate.
- Configurations that reshape the claim set — `[models].fast = None`
  (primary subsumes the fast role), single-model pods, the
  shared-model fleet — are invisible to call sites *by design*: they
  changed the claims, not the contract. Requests keep their declared
  requirements and the scheduler resolves against whatever is
  actually advertised.

## 9. Decision procedure for a new call site

1. Name the workload class (§3). Can't? The class list is wrong or
   the call is doing two jobs — split it or escalate to a human.
2. Take the class's requirement bundle; fill in honest budgets.
3. Machine-consumed output? Attach the schema/grammar and the
   feature requirement.
4. Construct via the `Workload` resolver (once landed) so the
   envelope, constraint, think budget, and tracing field are set
   together — or, until then, copy the enrich-client pattern and
   comment the class name.
5. If you believe the class table gives the wrong answer for your
   site, that is a policy change: this file changes in the same PR,
   with evidence.

## 10. Known debts against this policy

The rationalization that produced this policy (P1–P4) already closed
the structural problems it set out to fix: the ~60 free-hand `Speed`
literals now route through the `Workload` resolver (§9), the three
divergent Speed↔latency maps collapsed to `speed_to_latency` /
`latency_to_speed`, the `Speed::Medium` construction tier is retired,
and the grounding `model_id:"primary"` pins are gone (judges route
through the OICP envelope). What remains below is the residual — a
handful of *deliberate, scoped* deferrals, not oversights.

Every one is marked in code with a greppable comment stating the
current value, the mandated value, why it was preserved, and the
phase that owns the fix. Find them all with:

    grep -rn "POLICY-DEBT(" sovereign/

### Deferred phases (what a marker's "owning phase" means)

These are **not scheduled**. This project will not be revisited for a
while, so treat the markers as "safe to build near, but read the
marker before extending the site." New call sites must use the
`Workload` resolver (§9) and inherit correct defaults — the markers
only record where *existing* sites override back.

- **P5 — gray-zone re-adjudication.** Sites whose historical
  slot or `think_budget` differs from the mandated bundle and where a
  flip could change generation, so it needs a measured decision, not
  a mechanical swap. ("F5" is this finding in the 2026-07-08 audit;
  **P5-5b** is specifically the Fast-vs-primary judge-agreement
  measurement that gates the eval-judge flips below.)
- **P6 — defaults hardening.** The `Speed` derive default and a
  no-envelope burn-down: counting and eliminating call sites still
  sending `oicp: None` (invisible to the scheduler).
- **P7 — vocabulary collapse.** Fold `role.rs::Tier` into the
  `Workload` vocabulary and retire the `preferred_speed` shadow once
  it no longer rides on the request path.

### The residual markers, by kind

- **Think-budget neutrality — 9** (`memory.rs`, `pipeline/curator.rs`,
  `document.rs`, `raptor_atlas.rs`, `document_asset.rs` ×4,
  `corpus/mod.rs`). Each kept its historical `think_budget` (`None`
  vs `Some(0)`) instead of the bundle value, to keep the P1 migration
  generation-neutral. The two are equivalent at schema-constrained
  sites (no think block can precede forced JSON). Leaf knobs; they do
  not compound. Owned by P5.
- **Honest-budget forfeit — 4** (`document_operation.rs` 1024,
  `document_asset.rs` 768, `code/read_note_digest.rs` 800,
  `corpus/mod.rs` 4096). `max_output` exceeds the 512 fast-batch cap
  because the work genuinely needs the room, so the site forfeits the
  batched FastShort claim (§4.5). This is a correct trade-off, not an
  error; `corpus/mod.rs` notes the cap stops being load-bearing once
  grammar-constrained decode guarantees a valid array close.
- **Eval judges on the Fast slot — 2** (`eval_cmd/score.rs`). Judge-
  class work (should be Normal/primary) runs Fast in the eval
  harness. `SOVEREIGN_JUDGE_MODEL` forces primary when set; the
  default flip is gated on **P5-5b** measuring judge agreement first,
  so eval baselines don't shift silently.
- **Architectural non-migration — 1** (`typed_call.rs`). A generic
  helper whose speed / think_budget / temperature are caller-set
  `pub` fields read at runtime; a static `Workload` can't reproduce a
  per-call Fast override, so it deliberately stays off the resolver.
  Anyone extending the `Workload` model should know this opt-out
  exists. Owned by P7.

### Resolved after this policy shipped

- **2026-07-08 — memory-integrity guards flipped to primary.**
  Temporal-tension classification and contradiction detection in
  `memory.rs` previously ran on the Fast slot as a P1-neutrality
  holdover. They guard the durable memory store from corruption, are
  background tasks where latency buys nothing, and their
  `think_budget` already matched the ExtractDurable bundle — so they
  were moved to their mandated ExtractDurable (primary) class. The
  flip changed only the slot, not the generation.
