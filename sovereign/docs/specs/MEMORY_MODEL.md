# The Memory Model — Context as Working Memory, the Brain as Long-Term Store

**Status:** Initiative compass, drafted 2026-07-24. Principles (§3) are the
evaluation bar for all memory-adjacent work; experiments (§5) are the roadmap,
E1 in flight. Per `ARCH_PRINCIPLES.md §1.1`: §3 becomes contract as each
experiment's build log confirms it; until then treat any individual mapping
claim as a hypothesis with its measurement named.

**Owner context:** successor to (and generalization of) the bionic-suit
initiative. Everything remains accountable to `sovereign cache-audit`. Parent
specs: `SESSION_CONTINUITY.md` (the session frame = this model's index tier),
`cache-audit` (`--counterfactual`, `--ramp` = this model's measurement
instruments).

---

## 1. Thesis — we rediscovered the hippocampus for economic reasons

An LLM session's context window is working memory being abused as long-term
memory: append-only, no forgetting, and — unlike biological WM — charged
**rent on every token every turn** (cache-read ≈ avg_ctx × turns; measured at
76% of fleet session cost post-dedup, 2026-07-23). Humans pay ~zero rent on
LTM and a fixed cost on a tiny WM, and the resulting architecture is: reason
over **pointers and gists**, store content in vast external-to-attention
systems, reconstruct on demand, forget by predicted need.

Our optimization pressure is stronger than biology's (the rent is in
dollars *and* in long-context attention degradation), so we should expect the
same architecture to fall out — and empirically it has. The suit's components,
built purely from cost measurements, map onto the cognitive-science memory
model piece for piece (§2). This document makes the mapping explicit so it can
*generate predictions* instead of just flattering the design.

The one-sentence version a mid-level engineer can hold: **the context window
holds pointers and gists; the brain (notes, frames, facts, code graph) holds
everything else; retrieval is cheap, so eviction should be aggressive and
principled.**

---

## 2. The five stores — mapping table

| Cognitive system | Property | Our system | Status |
|---|---|---|---|
| Working memory (Cowan: ~4 chunks, chunks are pointers into LTM) | Tiny, expensive, where reasoning happens | Context window, target steady-state ≈ seed + active gist | Enforced only at split boundaries today; E3 is the gap |
| Hippocampal index (Teyler & DiScenna: stores pointers to cortical patterns, not content; recall = reinstatement) | Fast-written, small, per-episode | **Session frame** — spec §2's "pointers over prose" rule is the indexing property stated exactly | Shipped, split-safe (SESSION_CONTINUITY §3a) |
| Episodic LTM (specific experiences, fast write, decays) | One-shot encoding | Frames per session; transcript JSONL as the raw trace | Shipped |
| Semantic LTM (decontextualized regularities, slow consolidation) | Durable, generalized | Notes store (decision/invariant/attempt); commit harvest | Shipped |
| Neocortex / the environment itself (extended mind — Clark & Chalmers) | Vast, queryable, never "loaded" | Repo + SCIP graph + corpus: `symbols`/`callers`/`facts`/`code_search` | Shipped |

Consolidation (complementary learning systems — McClelland, McNaughton &
O'Reilly): episodic traces replay into the semantic store. Ours: notes written
at decision-time; commit harvest; the PreCompact distill hook is literally
*replay before the window is destroyed*. The §7 open question in
SESSION_CONTINUITY (should distill emit notes?) is the consolidation pathway
and should eventually ship.

---

## 3. Principles

**P1 — Pointers over prose, everywhere.** (Hippocampal indexing.) Any content
that exists in a queryable store enters working context as a pointer + gist,
never as a copy. Copies go stale and pay rent. Already the frame contract;
this principle extends it to *all* in-context state — tool results included
(P3).

**P2 — Write at encoding time; reconstruction has a ceiling.** (Generation
effect / levels-of-processing.) The agent holding the state writes
100%-fidelity traces; post-hoc distillation from the transcript measured 17%
recall. Traces written at the moment of the decision (notes) or the
transition (frame upserts) are the strong path; distillation is rescue
tooling and must never be load-bearing in a protocol. Consequence: invest in
encode-time tooling (E4a) before distill-prompt cleverness (E4b).

**P3 — Evict at work-item close.** (Fuzzy-trace theory: gist and verbatim are
parallel traces; verbatim is droppable once gist is banked.) When a work item
closes, its verbatim traces (file reads, tool output, build logs) are dead
weight — the note/commit/frame already banked the gist, and re-derivation via
code-intel is cheap. Splitting (SESSION_CONTINUITY §3a) is the coarse version;
E1 prices the fine-grained version; E3 builds it.

**P4 — Forget by need probability, not by TTL.** (Anderson & Schooler's
rational analysis: availability should track recency × frequency of use.) The
notes store's retention and the injection ranker should both run on measured
access statistics, not age. A note retrieved often and recently outranks a
newer never-retrieved one. E2.

**P5 — Dereference before load-bearing use.** (Confabulation control; DRM/
fuzzy-trace false memory is *gist-driven* — the more we run on gists, the more
plausible-but-false details the system will generate.) A pointer or gist
authorizes a query (`symbols`, `facts`, `notes`), never substitutes for one
when the answer feeds an edit, a commit, or a claim to the user. The grading
judge's hallucination penalty and the grounding gate are this principle in
existing clothes.

---

## 4. Evidence already banked

- **Gist-boot works at no measured quality cost:** successor session ramped on
  3,585 raw + 2,362 intel tokens, 0 repeat reads (cold baseline 10–55k, up to
  6 repeats) and immediately found a real bug in the donor's tooling
  (`a3e7e8bf`). SESSION_CONTINUITY §3a.
- **Rent dominates:** cache-read is 76% of actual fleet cost; splitting alone
  recovers 46.5–51.4%, nearly threshold-insensitive (H1).
- **Generation effect reproduced:** self-reported frame 100% recall vs
  distilled 17% (`svrn session grade`, golden-calibrated).
- **Interference relief reproduced:** the fresh-store successor caught the
  duplicate-usage bug the loaded donor session shipped past.
- **Batching is a minor lever** (H3 realizable ~4%): most serial small calls
  are stateful edit/build loops — genuinely sequential cognition, not waste.

---

## 5. Experiments

**E1 — Price eviction-at-close (H5 counterfactual). MEASURED 2026-07-24.**
H5 shipped in `cache-audit --counterfactual`: work-item close proxied by
`git commit` tool calls; on close, context returns to seed + ~1k gist per
closed item; the eviction re-prefills retained context once (5m cache write);
subsequent requests save the evicted cache-read.

Fleet result: **H5 = 5.8% ($40.71, 11 evictions) vs H1 ≈ 50%** — but the
per-session spread shows the constraint is *behavioral, not mechanical*:

- Sessions with zero in-transcript commits (verified: the two biggest,
  $62 and $176) score H5 = $0 — no boundaries, nothing to evict at.
- The one session practicing commit-per-work-item (34cf682b, 8 evictions):
  H5 = $29.27 = **72% of its H1** — without killing the session.
- Short sessions invert: 3fabc9ed H5 $0.85 > H1 $0.68 (a split's seed
  re-write doesn't amortize on short sessions; cheap evictions do).

Verdict: E3 is NOT justified as a splitting replacement on this number.
The durable findings instead: (1) **P3 is conditionally validated — its
value is gated on work-item closes existing in the transcript; commit
cadence is itself a memory-hygiene lever** (small, frequent commits create
eviction boundaries). (2) The dominant policy is **hybrid**: evict at close
where boundaries exist (5m write of ~50k retained, session survives), split
at threshold as the backstop for boundary-free growth. (3) Falsifiable
follow-up: as commit-per-item discipline spreads through the fleet
(CLAUDE.md protocol), H5's fleet share should climb toward the 34cf682b
ratio — re-measure in the weekly fleet report; if it doesn't move, boundary
density wasn't the binding constraint and this section is wrong.

**E2 — Rational forgetting for the notes store.** Need-probability ranking
(recency × retrieval frequency) for injection and retention, replacing pure
relevance match + the punted TTL policy. Requires retrieval logging first —
measure before tuning. Gate: injection hit-rate (injected note actually used
by the session) improves against the current hook's baseline.

**E3 — Live eviction mechanics.** Harness-side: on work-item close, replace
verbatim tool results with one-line gist + pointer. Blocked on E1's number.
Design constraint from the suit principle: hook/harness-enforced, never
model-discipline-dependent.

**E4 — Close the generation-effect gap (the 17% problem).** Three parts,
priority ordered by P2:
- **E4a (encode-time, strong path):** ship write-path 1 properly — a
  `session_state` upsert tool the agent calls at transitions (task start,
  step done, blocker hit), so the frame is *continuously* current and
  SessionEnd needs no LLM at all. The CLAUDE.md donor protocol is the manual
  version; E4a makes it a tool call. Gate: frames graded ≥70% with zero
  hallucinated verification claims, produced without a wrap-up prompt.
- **E4b (retrieval practice, weak path):** restructure the distill stage-2
  prompt from "summarize the spine" to "answer the eight section-questions,
  citing spine evidence per item" (testing effect / elaborative
  interrogation). Iterate against `svrn session grade`; baseline 17%.
- **E4c (richer encoding):** if E4b plateaus, the ceiling is the spine —
  enrich stage 1 (Edit outcomes, tool-call results summaries) before more
  prompt work. Prediction from P2: E4b+E4c together still land below E4a;
  measure to confirm, then set policy that distilled frames stay
  rescue-only regardless.

---

## 6. ROI ledger and the regime-change prediction

Banked: ~50% (H1 splitting, live as standing protocol). Candidate: E1/E3
eviction — upper bound to be measured; back-of-envelope, holding a ~150k-avg
session to ~50–60k steady state cuts the 76% rent line by roughly
two-thirds, overlapping heavily with H1 (the levers substitute more than
they stack on long sessions; E1 quantifies the residual).

**Prediction worth watching (falsifiable):** in the current regime the turn-0
preamble (~43k) is a 5% lever (H2a). In a gist-first steady state near seed,
the preamble becomes the *dominant* remaining rent, and the lever ordering
inverts — preamble diet and boot-brief tiering become first-class. If E1/E3
land and H2a's share does NOT grow in the next fleet counterfactual, this
model is missing something.

Costs honestly on the other side: encode-time writes (~2k tokens/frame + note
calls), reconstruction failures (caught by the `--ramp` gate), eviction
re-prefill (priced in H5), infra maintenance, and P5 discipline against
gist-confabulation.

---

## 7. Non-goals and risks

- **Not building:** vector-DB-of-everything, whole-transcript RAG, or any
  "load more context" path. The model says load *less*, better-indexed.
- **Risk — gist confabulation (P5):** more gist-reasoning means more
  plausible-but-false detail generation. Mitigation is cultural + structural:
  dereference-before-use, hallucination-penalized grading, grounding-gate
  posture. If successor error rates rise as eviction gets aggressive, this is
  the first suspect.
- **Risk — the mapping seducing us.** Cog-sci is the compass, cache-audit is
  the terrain. Every E-item carries a measured gate; when the analogy and the
  measurement disagree, the measurement wins.
