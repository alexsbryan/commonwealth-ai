<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# gap.rs retirement — findings & decision (I4-C, 2026-07-19)

The plan (EPISTEMIC_STATE.md / RETRIEVAL_REDESIGN) proposed retiring
`sovereign-core/src/gap.rs`'s LLM `identify_gap` and replacing gap
*detection* with the deterministic demand/coverage residue
(`finish_demands`), keeping an LLM pass only for *phrasing*. "Retirement
is a goal, not a dogma — if parity fails, gap.rs stays and a decision
note records why." This is that note.

## Fixture bank

`bank.toml` — 12 `(question, answer, evidence)` triples curated from a
real chaos-monkey run (secret_agent bank / chaos-secret-agent corpus,
2026-07-19), 6 `has_gap=true` / 6 `false`, each with a `gap_kind`:

- **retrieval** — the answer IS in the corpus but was not retrieved
  (e.g. *present-wife*: Winnie is Verloc's wife, in the novel). An
  external info-request would NOT help; the fix is retrieval.
- **knowledge** — genuinely absent from the corpus (e.g.
  *ood-python-linkedlist*, *absent-professor-realname*). An external
  source IS the remedy.

## Finding 1 — the deterministic VERDICT is a faithful detector; the gap-rows residue is not

Scored on the 12-triple bank:

| detector | has_gap accuracy |
|---|---|
| `verdict == cannot_know_from_here` | **12 / 12** |
| `gaps` non-empty (`finish_demands` residue) | 11 / 12 |

The residue misses **`distract-money-keeper`** — a correct abstention
that carried **zero gap rows** (`verdict=cannot_know_from_here, gaps=[]`;
`present-maximal-statepower` is a second such case in the run). So the
plan's literal proposal — detect via *the gaps residue* — is slightly
wrong. The right deterministic detector is **the verdict** (which is
itself derived from the same abstention signal, and just cleared the
chaos parity gate at 43/43). Detection can retire to the verdict.

## Finding 2 — the coverage_probe misclassifies topic-vs-claim on a sealed corpus (blocks ROUTING retirement)

`gap.rs`'s card also carries *routes* (where to get the missing thing),
driven by the topic-vs-claim distinction. The deterministic equivalent
is `coverage_probe`'s `TopicUncovered` (no corpus near it → acquire an
external source) vs `ClaimUncovered` (corpus has the topic → deeper
retrieval). On this sealed single corpus the probe collapsed almost
everything to `claim_uncovered`, INCLUDING genuine knowledge gaps:

| question | truth | coverage_probe said |
|---|---|---|
| ood-australia-capital | topic-uncovered (novel has nothing on it) | **claim_uncovered** ✗ |
| ood-berlin-wall | topic-uncovered | **claim_uncovered** ✗ |
| ood-python-linkedlist | topic-uncovered | claim_uncovered ×2, topic ×1 |

Root cause: the probe's `nearest_vector_distance` floor (0.55) was tuned
for MULTI-corpus installs. On a dense single corpus every query finds
*some* nearest chunk above the floor, so the probe almost always reports
"an installed corpus is near this topic." It cannot distinguish a
retrieval gap from a knowledge gap here.

> **CORRECTION (measured 2026-07-19, `coverage_probe_scope_fix`).** The
> above root cause was WRONG. Instrumenting the probe (best_corpus +
> best_similarity per turn) showed the **floor is well-calibrated**: OOD
> queries land at 0.17–0.49 cosine, in-topic gaps at 0.71 — a clean split
> at 0.55. Two *different* bugs caused the misclassification: (1) the probe
> fanned across an arbitrary first-12 of `installed_indexes()` — on a
> sealed novel turn OOD queries matched *unrelated* installed corpora
> (python→a code corpus, margarita→a conversations corpus), so the verdict
> was non-deterministic; (2) `unverified` turns (a released decline over
> distractors, e.g. australia/berlin) have `gap_turn=false`, so the probe
> never runs and coverage *defaults* to `ClaimUncovered`. **Bug 1 is fixed**
> by scoping the probe to `enabled_corpora` (deterministic; sims now reflect
> the sealed corpus; ~10× faster). **Bug 2 remains** — its clean fix is
> upstream (the gate should abstain, not release, a 0-holding decline), not
> a coverage change.

## Decision

- **Retire gap DETECTION → the deterministic verdict.** Parity-strong
  (12/12 on the bank; the verdict itself passed the 43/43 chaos parity).
  An LLM audit over windowed inputs has a documented false-positive
  history (the Einstein four-papers case, gap.rs:33-40); a deterministic
  detector that reads the already-computed verdict is both cheaper and
  at least as accurate.
- **Do NOT yet retire gap.rs's ROUTING/phrasing.** The `coverage_probe`
  must be recalibrated for sealed/single-corpus turns first (a
  per-corpus floor, or a "sole sealed corpus ⇒ topic-uncovered when the
  answer isn't in it" rule) — otherwise the deterministic path emits
  worse routes than the LLM card. Recalibration is the gating follow-up.

**Net: gap.rs stays for now, but its detection is superseded by the
verdict.** The clean retirement is a two-step: (1) route the
`maybe_collaborate` gap hook off `identify_gap` detection and onto the
verdict + a phrasing-only pass; (2) recalibrate `coverage_probe` so the
routes match. Step 1 is safe today; step 2 is the prerequisite for
deleting gap.rs entirely.

## Remaining verifiable work

- A Rust loader + `bench gap-check` scorer that also runs `identify_gap`
  (daemon-backed) on the bank, to confirm the LLM detector does not beat
  12/12 (confirmatory — the deterministic case already stands).
- The `coverage_probe` sealed-corpus recalibration + its own A/B.

## EXECUTED (2026-07-20) — gap.rs deleted

Both retirement steps landed:

1. **Detection → the verdict's signal.** `run_collaboration` now takes
   `abstained: bool` — the gate signal `TurnVerdict::CannotKnowFromHere`
   derives from (D3) — as its ENTIRE detection. Answered turns pass
   through instantly (no more 15-55s grammar-constrained fast-slot audit
   per answered turn). The card's ask is a phrasing-only fast-slot pass
   (`phrase_gap_question`, D4: may phrase, never invent; hard fallback =
   the user's question verbatim). Post-stream callers derive the signal
   from the persisted `grounding_gate.action`; non-streaming handlers
   thread it from their gate outcome. The doc-op attached-doc path runs
   no gate → carries no signal → never fires the card (its short-answer
   cases already fall through to the gated runtime pipeline).

2. **Routing blocker closed.** The CORRECTION above stands: bug 1
   (arbitrary fan-out) was fixed by scoping the probe to
   `enabled_corpora`; bug 2 (released declines never probe) is fixed
   UPSTREAM by the gate's decline guard (`released_pure_decline` in
   `grounding/mod.rs`, 2026-07-20): a NO_CLAIM release whose text is a
   pure provenance-flagged decline is reclassified `abstained_decline`,
   so the turn derives `CannotKnowFromHere`, the probe runs, and
   topic-vs-claim routes correctly. Caveated parametric ANSWERS
   ("Not in your sources — from general knowledge: …") are structurally
   excluded and keep releasing.

`gap.rs` (LLM judge, windowed inputs, saturation guard) is deleted; the
`InformationRequest` DTO survives as the card's view-model per plan §6.
The "Rust loader + bench gap-check scorer for identify_gap" follow-up
dies with the judge (nothing left to confirm). The bank stays as the
regression fixture for the deterministic detector's recorded
`det_verdict` outcomes.
