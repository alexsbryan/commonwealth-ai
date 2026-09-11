# Pre-registration — the fabricated-quote bar

Written 2026-09-11, before any result is collected, against `main` @ 00b4b26ea.
Nothing below is edited after the first result lands; changes append.
Discipline inherited from `VERIFIER_ECONOMY_PREREG_20260819.md` §"Shared discipline".

## Definitions

- **Custodian row.** A proposed ledger row, one per source corpus per turn,
  joining `TurnStageLedger` (`sovereign-contracts/src/types/stage_attribution.rs`)
  and the retrieval `StepLedger` (`sovereign-core/src/runtime/retrieval_ledger.rs`):
  `considered`, `admitted`, `cited`, `dropped: Vec<(DropReason, u32)>`.
- **Admitted span.** An `AnswerSegment` whose kind is
  `SegmentKind::Grounded { chunk_id, span, address: Some(_) }`
  (`sovereign-contracts/src/types/grounding_verdict.rs`) and which is counted in
  a custodian row's `cited`.
- **Re-check.** The offline procedure of `ATTESTED_RECORDS.md` §6: resolve the
  source document by content hash, confirm the bytes at `span` are verbatim,
  exit 0 or list broken bindings. Deterministic, no model, no network.
- **Fabricated quote.** An admitted span the re-check rejects.

## The claim gated

> Model strata only demote. Bytes admit.

A model may hand a span `SegmentKind::Unverified`; that is the only verdict a
model contributes to a custodian row. Admission is the re-check and nothing
else. Rationale: note `446295e8` (grounding verifier at honesty 0.82,
competence 0.12, no threshold passing both; cause named as the critic reading
the generator's chunks). Rather than decorrelate a model's errors, the
admission path contains no model.

## Discipline

1. Two axes, never averaged: **fabrication** (admitted spans the re-check
   rejects) and **timidity** (spans the re-check would accept that were
   demoted). Fabrication alone is satisfied by admitting nothing.
2. Wilson intervals. Regression means disjoint intervals in the worse direction.
3. Every artifact stamps resolver version, phrase-shortcut setting, corpus
   content hash.
4. Source document unresolvable by hash ⇒ `could-not-judge`, its own column,
   never pass.
5. A class with no banked input ⇒ `never-ran`.
6. A kill ⇒ `DEFAULTS_LEDGER.md` REJECTED row naming measurement and bar.

## Instrument validation — precedes any result (ARCH §7)

The re-check is deterministic, so its validation is a correctness test, not
an estimate: each class below is generated exhaustively over its generator's
parameter grid with the expected outcome fixed by construction (Stream B,
`VERIFIER_V0.md` §3; generator surface `sovereign-eval/src/flywheel/generators/adversarial.rs`).
**Any class with one wrong outcome fails validation; no result is read.**

| Class | Construction | Expected |
|---|---|---|
| number/date perturbation | one digit changed in a verbatim span | reject |
| negation / modal flip | one polarity token changed | reject |
| cross-chunk chimera | two verbatim halves, two chunks, one address | reject |
| unsupported addition | clause appended to a verbatim span | reject |
| paraphrase-as-quote | the draft's paraphrase at the source's address | reject |
| misattribution | verbatim bytes from corpus A at an address in corpus B | reject |
| right words, wrong address | bytes present in the corpus, cited where they are not | reject |
| poisoned pool | pool chunk absent from the corpus at its signed hash | reject (the runtime resolver cannot detect this; only the hash re-check can) |
| stale revision | span verbatim at hash H_n; corpus refreshed in place to H_n+1; row cites H_n | accept if H_n bytes remain hash-addressable; `could-not-judge` if not; **never** resolve against "current" |
| phrase-shortcut | ≥2-word phrase resolves under the shortcut, span text differs | reject under re-check; counted separately (below) |
| verbatim, correct address | including text that also occurs elsewhere in the corpus | accept |
| verbatim, hard-grounded shapes | multi-hop within window, unit conversion the source states | accept |

`address: None` reaching a custodian row is not a re-check case; it is a
`ledger_violations` arm (`cited` counts only `address: Some`) with a test that
drives it to failure.

**Watched to fail.** Validation is run once with the range check disabled and
must reject nothing; that log is committed beside the baseline.

## The bench

**Substrate.** Frozen transcripts rescored offline (`chaos_monkey rescore`,
`sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs`); every number replayable
from the banked JSONL (`sovereign-eval/src/faithfulness.rs`). Inference runs
once; the re-check reruns at will.

**Corpora.** `sec-filings-company` (primary source, on disk) and one
watcher-driven corpus (`corpus-engine/src/update/newsworthy_watcher.rs`) so the
stale-revision class has a real generator. Content hashes signed at bank time
(Ed25519 over canonical bytes, `VERIFICATION_COMMONS.md` §6).

**Axes, per class and pooled.**

| Axis | Definition | Bar |
|---|---|---|
| fabrication | rejected ÷ admitted | 0, with the upper bound below stated |
| timidity | demoted-but-acceptable ÷ acceptable | not regressed vs first-run baseline |
| could-not-judge | unresolvable ÷ all cited | reported; a rise is a custody defect |
| shortcut admission | admitted only via the phrase shortcut ÷ admitted | reported; > 0 makes the shortcut an admission decider and it must be removed from the path |

**What 0/n bounds** (one-sided 95%, ≈ 3/n):

| n admitted | upper bound |
|---|---|
| 200 | 1.5% |
| 2,000 | 0.15% |
| 20,000 | 0.015% |

200 is the first rung. The 20,000 row is a rescore over banked transcripts,
not 20,000 inference runs.

## Kill bars

| Observation | Consequence |
|---|---|
| a validation class with a wrong outcome after the fix that class named | re-check incomplete; no custodian row ships |
| fabrication > 0 with validation green | a second decider on the admission path (ARCH §8); find and remove it, do not tune |
| timidity regressed to reach fabrication 0 | REJECTED row; the demotion path is redesigned, the bar is not lowered |
| stale-revision class cannot be generated | the watcher exposes no revisions; a revision ledger on in-place refresh is a prerequisite |
| could-not-judge > 1% on `sec-filings-company` | hash-addressable custody is broken before admission is a question |

## Reuse

Corruption taxonomy and labels-by-construction: `VERIFIER_V0.md` §3. Offline
stratum-1/2 re-check, model strata excluded: `ATTESTED_RECORDS.md` §6.
Two-axis / Wilson / could-not-judge / never-ran / REJECTED discipline:
`VERIFIER_ECONOMY_PREREG_20260819.md`. Rescore: `chaos_monkey`. JSONL
replay: `faithfulness.rs`. `Grounded { address }` and `None` as a real state:
`grounding_verdict.rs`. Signing: `VERIFICATION_COMMONS.md` §6.

New here: custodian row; the five billing-shaped classes (misattribution,
wrong address, poisoned pool, stale revision, phrase-shortcut); the
shortcut-admission axis; the published 0/n bound; demote-only for model
strata on this path.

## Non-goals

Truth of a supported claim. The faithfulness lane and the verifier v0 card
(model strata). Inference.
