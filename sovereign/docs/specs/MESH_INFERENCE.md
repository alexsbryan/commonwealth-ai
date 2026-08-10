# Mesh Inference — strong-peer topology, claims vs. codebase

Status: **assessment + corrected build order** (2026-06-10). Source: the
"latency-class hierarchy" design memo (hub-and-spoke: one strong box,
many laptops). Every claim below was checked against the code; this doc
records what already exists, what the memo gets wrong about THIS
codebase, and the build order that survives contact with it.

The memo's central insight stands: **don't add weak peers' FLOPs to the
strong peer's forward pass — treat the mesh as a latency-class
hierarchy** (hide latency to the hub, shed demand away from it, exploit
locality on it, keep spokes busy with latency-tolerant work).

---

## §0. The win we're buying — the end-of-project demo

One sentence: **"One big box serves the whole house at once."**

Today the hub is effectively single-tenant. Simple questions burn its
capacity, a large spliced prefill freezes someone else's stream,
background enrichment competes with chat, and every turn from every
device lands on it. The project is worth doing iff, at the end, this
scripted scenario demonstrably works — on video and in a bench:

**The household scenario (`household-bench`):** three concurrent
actors against one hub —
- **Alice** kicks off a corpus enrichment / RAPTOR build (background
  LLM-heavy work on the hub).
- **Bob** (spoke laptop, normal Wi-Fi) runs complex knowledge queries
  that genuinely need hub-quality synthesis.
- **Carol** (spoke laptop / phone) chats a realistic casual mix —
  mostly simple turns, occasional complex ones.

**Falsifiable end-state hypotheses** (each maps to an increment; if
the measurement says no, that increment doesn't ship):

| # | Hypothesis | Target | Increment |
|---|---|---|---|
| H1 | A large share of household turns never touch the hub, with no perceptible quality loss | ≥50% of the mixed-turn bank routes spoke-local; LLM-judge delta on those turns ≤0.02 vs all-hub routing (within established judge noise); zero humility-bank regressions | Cascade (Inc 2) |
| H2 | Nobody's chat stutters because of someone else's work | Bob's p95 TTFT under full household load ≤1.5× idle-hub TTFT; zero mid-stream stalls >500ms while Alice's enrichment runs | Queue classes + HOL guard (Inc 4) |
| H3 | Hub-quality answers get faster, then more plentiful | (a) single-stream 35B decode ≥1.5× via hub-side colocated speculation (4B drafts for 35B — both already resident); (b) only if concurrency demands it: ≥2 simultaneous hub-quality streams each ≥0.7× solo rate via spoke-side drafting | Spec-dec (Inc 3) |
| H0 | We can prove all of the above honestly | `household-bench` runs the 3-actor scenario reproducibly and reports per-actor TTFT/rate/judge before ANY optimization lands | Baseline first (Inc 0) |

The demo at the end: run `household-bench` on the pre-project build
and the final build, side by side. Alice's build finishes, Bob's
streams don't stutter, Carol never noticed she was mostly talking to
her own laptop — and the numbers say so.

**A correction this crystallization surfaced** (changes Increment 3's
justification): the memo's "one RTT per token" framing for plain
remote streaming is wrong for THIS architecture. Sovereign streams
over SSE/WS — the server pushes tokens as generated, so WAN RTT hits
*TTFT*, not inter-token rate. Therefore draft-on-spoke does NOT beat
hub-side colocated speculation for a single stream; what spoke-side
drafting buys is **hub capacity under concurrency** (drafting compute
moves to the spokes; the hub runs only cheap batched verify passes).
Build order inside Increment 3 follows: hub-side spec-dec first
(H3a — zero mesh complexity, models already colocated), spoke-side
drafting second and only if H3b's concurrency demand is real. The
RTT-per-token regime the memo describes exists only in layer-sharded
decode — which stays the regime of last resort.

---

## 0. Scoreboard — memo §§ vs. reality

| Memo § | Proposal | Reality | Verdict |
|---|---|---|---|
| §1 | Layer-sharding = fallback, not default | **Already implemented exactly so.** `resolve_placement` (sovereign-inference `rpc_distribution.rs:600-723`): LocalOnly default; StreamSplit ≤500MB; owned-override sharding only for Primary + large model + orchestrator/assume-warmed. Fast/Embed slots never shard. | ✅ Done — no work |
| §2 | Draft on spoke, verify on hub | No mesh primitive today (MTP is intra-slot), **but easier than the memo claims** — see §2 below | 🔶 The real project, upgraded |
| §3 | Confidence-gated cascade w/ fidelity cards | Scaffolding exists (Intent+confidence, effort escalation, role::Tier); local-vs-peer is binary today; **fidelity cards have ZERO runtime consumers** and thin coverage | 🔶 v0 cheap, card-gating later |
| §4 | Slot-pinned prefix caching, "days, immediate win" | **Wrong on this fleet** — see §4 below | ❌ Reshaped |
| §5 | Hub queue priority classes + contribution-weighted fair share | Admission is binary 503 (pause/yield/ceiling); **contribution-weighting contradicts the ledger's own design** — see §5 | 🔶 Classes yes, weights no |
| §6 | Spokes = storage/embedding/enrichment tier | Shards + remote embeddings already live; `usage_predictor` exists but predicts demand TYPE, not idle, and **has no consumers** | 🔶 Wiring work |
| §7 | Sharding heuristics (min cuts, hub gets head, latency-ordered chain) | `layer_assignment.rs` already does contiguous ranges + entry-node preference; **`LatencyMatrix` exists but NOTHING consumes it** | 🔶 One cheap wire-up |

---

## 1. What the memo got wrong about this codebase (the load-bearing corrections)

### ❌ §4's "immediate TTFT win" doesn't exist on the deployed fleet

Two independent blockers:

1. **The canonical primary is Qwen3.5-35B-MoE, and prefix-cache reuse
   is disabled for it BY DESIGN.** `prefix_cache_safe` gate
   (`model_slot.rs:1391`, arch detection `prompt_helpers.rs:92-114`):
   qwen-MoE's gated DeltaNet recurrent layers cannot survive partial
   KV-keep (`clear_kv_cache_seq` returns -1) → `lcp = 0`, full clear
   every turn. Pinning conversations to slots buys nothing the engine
   is allowed to keep. (Established invariant, 2026-05-29: "don't tune
   prompt-layout / concurrency for cache on Qwen-MoE; spend on shorter
   generations.")
2. **Even on cache-safe models the shared prefix is small.** The
   assembled prompt is system → tools → evidence → history → turn, and
   the *evidence section changes every turn* (per-query retrieval).
   Cross-turn LCP ≈ the static system preamble (hundreds of tokens),
   not the 10k-token spliced context the memo imagines. Making the
   prefix stable means moving evidence BELOW history in the layout — a
   measured prompt-engineering project with bench gates (layout changes
   move synthesis quality), not a cache toggle.

What survives: per-slot LCP reuse already works for cache-safe models
(Fast slot, non-MoE primaries); a conversation→hub stickiness term in
OICP is still cheap and future-proofs the day a cache-safe primary
lands. But it is not the first move, and nothing here is "days for an
immediate win."

### ✅ §2's draft/verify is EASIER here than the memo claims

The memo: "llama.cpp doesn't expose this remotely — a real PR." But the
hub doesn't run llama-server for local serving — it runs **our embedded
engine** (llama-cpp-4). The `/internal/verify` endpoint is ours to add:
batch-decode k draft tokens in one pass, compare against target
sampling, return accept-length + correction. No upstream PR.

Three more tailwinds the memo couldn't know:

- **The canonical fleet is already a draft/target pair.** Primary
  35B-IQ4 + fast 4B-Q8 are both Qwen3.5 — same tokenizer family. The
  "vocabulary identity" hard constraint is likely free on the exact
  deployment that matters. (Verify-compat must still be CHECKED and
  advertised — the OICP `verify_compatible_with` field stays in scope.)
- **Speculation plumbing exists in-slot.** MTP
  (`SlotInferenceMode::Speculative`, `generate_sync_mtp`,
  `model_slot.rs:2220+`) already does draft+verify loops, acceptance
  handling, and failure quarantine — the *shape* of the verifier
  (batched parallel pass over candidate tokens) is implemented; what's
  missing is decoupling draft from verify across the network.
- **The baseline is WAN-RTT-dominated** (the published negative results
  for speculation are local-baseline regimes), and
  `ThroughputObservedStream` (`throughput_tracking.rs`) is the natural
  home for per-(draft,target) acceptance-rate EWMA with fallback to
  plain streaming.

Interactions to respect: MTP and remote-draft verification are both
"speculative_active" regimes — one slot can't do both at once; and the
MTP gate already skips when tools are set (grammar-constrained sampling
× speculation needs explicit handling — same class of issue as the
alternation-grammar P0).

### ⚠️ §5's contribution-weighted fair share contradicts the ledger's charter

`NodeContributions` (commonwealth-core `contributions.rs:143-148`)
*deliberately* carries "no balance field, no exchange rate, no ranking"
— contributions are dimensional facts for operators, not a score. That
is a values decision (anti-commodification), not an oversight.
Collapsing it into scheduler weights would re-introduce exactly the
ranking the design refuses. **Priority CLASSES (interactive decode >
interactive prefill > verify batches > peer batch > background
enrichment) are in scope; contribution-weighted shares are not** unless
the project deliberately revisits that charter.

### 🔍 Two things exist but are dead wires (cheap wins hiding in plain sight)

- **`LatencyMatrix`** (commonwealth-core `latency.rs`) — pairwise
  RTT/jitter/bandwidth, gossiped… and consumed by *nothing*. OICP's
  `locality_bonus` uses a coarse Local/Near/Far enum instead. Wiring
  the matrix into locality scoring (and §7's chain ordering) is a
  contained, test-friendly change.
- **`usage_predictor`** — (weekday, hour) → capability-type
  distribution, consumed by *nothing*, and it does NOT predict idle.
  §6's "schedule enrichment into the hub's idle hours" needs an
  idle-hours signal added (the foreground-yield timestamp stream is the
  obvious source) plus an actual consumer in the enrichment scheduler.

### 📇 Fidelity cards: real artifact, not yet routing-grade

`FidelityCard` exists (`~/.svrnmesh/model-fidelity-cards/<model>.json`,
Grade per (model, mechanism-class) with confidence + provenance) and
nothing at runtime reads it. The cascade coupling (§3) is genuinely
novel — but today's card corpus covers the mechanism-fidelity bench's
three reasoning classes, which is too thin to gate live routing. v0
cascade should ship on confidence/effort/tier (all existing signals);
card-gating becomes load-bearing as the card corpus grows past
bench-demo scale.

---

## 2. Corrected build order

Each increment independently landable, bench-gated, and honest about
what it can't know until measured. Every increment's exit criterion is
one of §0's hypotheses.

### Increment 0 — `household-bench` baseline (FIRST, before any optimization)
- The 3-actor scenario harness (§0): scripted Alice-enrichment +
  Bob-complex + Carol-casual against one hub; reports per-actor TTFT
  p50/p95, inter-token rate, stall count, hub-vs-local turn share, and
  judge scores on a fixed turn bank. Composes existing pieces
  (ci-bench runner discipline, marathon-style multi-turn fixtures,
  ThroughputObservedStream metrics).
- Run it on TODAY'S build and commit the numbers. This is the "before"
  of the demo and the null hypothesis for every later increment —
  without it, H1–H3 are vibes.

### Increment 1 — wire the dead signals (small)
- `LatencyMatrix` → OICP `locality_bonus` (replace the 3-bucket enum
  with measured RTT when a matrix entry exists; keep the enum as
  fallback). Also feeds §7's chain ordering for free.
- Per-(draft,target)-shaped acceptance/throughput bookkeeping slots in
  `NodeObservations` (schema only — consumed by Increment 3).
- Glassbox: score breakdown already logs; add the matrix term.

### Increment 2 — cascade v0 in `decide_policy` (medium, all parts exist)
- Thread a **hardware tier** into the policy output: high-confidence
  `SimpleQuery`/`KnowledgeQuery` → spoke-local Main slot; low-confidence
  or `ComplexTask`/`DeepQuery` (incl. effort escalation, which already
  exists) → hub via MeshInferenceProvider. Today's binary local-first
  becomes a explicit, classified choice.
- Bench gate: ci-bench core + judge deltas with before→after numbers;
  the chaos-monkey humility bank guards the "cheap model confidently
  wrong" failure mode.
- Fidelity-card gating = v1, behind a flag, once cards cover enough
  (model, class) pairs to beat the confidence baseline. (Card→runtime
  read path is new code either way; build it flag-gated in v1.)

### Increment 3 — speculation, in two honest stages (the project)

**3a — hub-side colocated spec-dec (H3a).** The 4B fast model already
lives on the hub next to the 35B primary; classic draft/target
speculation between them accelerates every hub stream with zero mesh
complexity. Per-class acceptance EWMA; automatic fallback when
speculation isn't paying; the MTP quarantine pattern is the template.
Exit: single-stream decode ≥1.5× on knowledge/templated workloads.
(Interaction: a slot runs MTP *or* draft/target, not both;
tools/grammar requests bypass speculation — same gate as MTP's.)

**3b — draft-on-spoke / verify-on-hub (H3b), only if concurrency
demands it.** Justified by hub *capacity* (drafting compute moves to
spokes; hub runs cheap batched verify passes), NOT single-stream
latency — see §0's correction.
- `/internal/verify` on the hub: embedded-engine batched verification
  of a k-token draft block (sequence-level; tree-structured only if
  acceptance data demands it). Our endpoint; rides the PeerTransport
  seam.
- OICP: `verify_compatible_with` on `CapabilityClaim`
  (tokenizer/family fingerprint), checked at pairing time.
- Spoke loop: Fast slot drafts k, ships block, applies accept-length +
  correction; per-(draft,target) acceptance EWMA with fallback to
  plain remote streaming.
- Exit: ≥2 simultaneous hub-quality streams each ≥0.7× solo rate in
  `household-bench`. If 3a + cascade already keep the hub unqueued at
  household scale, 3b doesn't ship.

### Increment 4 — hub queue discipline (medium)
- Priority classes on the hub's serving path: interactive decode >
  interactive prefill > verify batches > peer batch > background.
  Foreground-yield already implements the top/bottom split; this adds
  the middle tiers + admission awareness of request class.
- HOL guard: admission-gate large prefills (spliced contexts) behind
  in-flight interactive decode — "Alice's RAPTOR build freezes Bob's
  chat" is the failure mode to test for explicitly.
- NO contribution-weighted shares (see §1); revisit only as a
  deliberate values decision.

### Increment 5 — spokes' real jobs (wiring)
- Idle-hours signal (from foreground-activity stream) + make
  `usage_predictor` a consumer-facing input to enrichment scheduling
  (hub does LLM phases off-hours; spokes do extraction — the
  enrichment pipeline already splits phases this way).
- Shards + remote embeddings already live; add rerank-as-a-service only
  when a workload demands it.

### Deferred / reshaped
- **Prefix-cache locality (memo §4):** conversation→hub stickiness term
  in OICP can ride Increment 1 cheaply (helps observed-throughput
  locality today, KV locality someday); slot-pinning + evidence-below-
  history prompt layout only when a cache-safe primary is deployed, and
  only with bench gates on synthesis quality.
- **Prefill/decode disaggregation:** LAN-only per the memo; no current
  LAN-10GbE pair in the fleet to justify it.
- **PETALS-style redundant sharding:** out of scope; flap-quarantine +
  supervision (already shipped) is the pragmatic answer.

---

## 3. What keeps it honest

The measurement stack the memo asks for already exists: `LatencyMatrix`
(once wired), `ThroughputObservedStream` EWMAs, ci-bench core gate,
chaos-monkey calibration bank, and fidelity cards as they grow. Every
increment above lands with a before→after number from one of these or
it doesn't land.
