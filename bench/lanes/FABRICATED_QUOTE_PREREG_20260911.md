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

## Where it lives

Not a sibling lane. The bench README's rule holds: the suite composes. This
lands as one tracked quantity in the chaos-monkey lane,
`span_binding_failure_rate`, beside `caveated_fabrication_rate`, scored by
the same `rescore`; the validation classes are pure Rust tests in
`sovereign-eval` beside `flywheel/det_checks.rs`.

## Substrate — state as found 2026-09-11, and what each ring needs

Nothing banked carries a span address today. Every chaos-monkey results file
has zero `answer_segments`; the forensics ledgers carry chunk text with no
ids or hashes; the index doc struct has a `content_hash` field whose three
writers (`corpus-engine/src/index/mod.rs`) all set `None`; the resolved
address at `sovereign-core/src/runtime/streaming.rs` is a pool index plus
byte range with no corpus identity. Prerequisites, in order, each named by
the ring it unblocks:

| # | Prerequisite | Size | Unblocks |
|---|---|---|---|
| 1 | write `content_hash` at the three index sites | small | all |
| 2 | resolved address carries corpus chunk id + hash (streaming site; `segments_for_display` takes ids, not bare texts) | medium | ring 0 address classes |
| 3 | the re-check fn + five new `SiteWitness` variants in `flywheel/generators/adversarial.rs` | medium | ring 0 |
| 4 | bench harvest and flip-soak journal write `answer_segments` + sealed-pool ids/hashes | small | rings 1, 3 |
| 5 | span-binding quantity in `chaos_monkey rescore` | small | ring 1 |
| 6 | watcher retains prior revisions | Phase-3 work | stale-revision class; `never-ran` until then |

Rings, innermost first. Iterate in 0; everything outward is a refresh.

| Ring | Runs | Cost | Inference |
|---|---|---|---|
| 0 | re-check + every validation class, generated by `generate_cases` from a banked harvest (the faithfulness obsidian seeds are already `HarvestItem`-shaped) | seconds, `cargo test` filtered | none |
| 1 | `span_binding_failure_rate` and timidity, rescored over banked transcripts | seconds to a minute | none |
| 2 | chaos-monkey banks (secret agent 43 q, saltgrass 42 q, sep-chaos, + one on `uap-blue-book-scans` for real OCR garble) on both fleet tiers, nightly | ~1 h per bank per machine (median 72–100 s/question measured 2026-08-14) | yes |
| 3 | every fleet turn in normal use, via the flip-soak journals | free | dogfooding |

Fleet as found: this host M2 Max 64 GB (Qwen3.5 4B/2B Q6_K, 0.6B embedder;
35B-A3B is a partial download here); Strix Halo 128 GB unified running
35B-A3B at ~46 tok/s (note `8a92e75e`; not reached this session); rented L40S
via `pod up`.

**Axes, per class, per corpus, and pooled.**

| Axis | Definition | Bar |
|---|---|---|
| fabrication | rejected ÷ admitted | 0, with the upper bound below stated |
| timidity | demoted-but-acceptable ÷ acceptable | not regressed vs first-run baseline, per corpus, never pooled |
| could-not-judge | unresolvable ÷ all cited | reported; a rise is a custody defect |
| shortcut admission | admitted only via the phrase shortcut ÷ admitted | reported; > 0 makes the shortcut an admission decider and it comes off the path |

**What 0/n bounds** (one-sided 95%, ≈ 3/n), and which ring reaches it. At
~7 claims a turn and ~half admitted, a turn yields 3–4 admitted spans.
Synthetic ring-0 spans never count toward n: a bound on constructed cases is
the instrument grading itself.

| n admitted | upper bound | source |
|---|---|---|
| 200 | 1.5% | one nightly bank (ring 2) |
| 2,000 | 0.15% | a week of fleet turns on two tiers (ring 3) |
| 20,000 | 0.015% | months of ring 3, or a rented pod for a weekend; never 20,000 chaos questions |

## Heterogeneity guards

The re-check is deterministic and cannot overfit. The demotion path can: it
is a model decision and can be tuned until timidity looks good on the bank it
was tuned on. Four guards:

1. Two generator tiers on two machines (4B on the M2 Max, 35B-A3B on the
   Halo); a single model's failure style is never the whole bank.
2. Four corpus shapes: fiction (secret agent, saltgrass), philosophy
   (sep-chaos), scanned OCR (`uap-blue-book-scans`), primary-source finance
   (`sec-filings-company`, public domain, not yet installed).
3. One corruption kind and one corpus held out entirely, under the
   peek-budget ledger pattern
   (`sovereign/bench/enron/baselines/enron-entity-resolution/peek_budget.json`
   is the pattern).
4. Timidity reported per corpus. A demotion rule that buys honesty on
   fiction by going silent on filings is two numbers moving apart.

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
`VERIFIER_ECONOMY_PREREG_20260819.md`. Rescore and the tracked-quantity slot: the chaos-monkey lane. JSONL
replay: `faithfulness.rs`. Peek budget: the enron lane. `Grounded { address }` and `None` as a real state:
`grounding_verdict.rs`. Signing: `VERIFICATION_COMMONS.md` §6.

New here: custodian row; the five billing-shaped classes (misattribution,
wrong address, poisoned pool, stale revision, phrase-shortcut); the
shortcut-admission axis; the published 0/n bound; demote-only for model
strata on this path.

## Non-goals

Truth of a supported claim. The faithfulness lane and the verifier v0 card
(model strata). Inference.
