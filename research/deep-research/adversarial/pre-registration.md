# Pre-registration record — the changed gate's adversarial read

§18.6 discipline (gate-redesign.md §5): the frozen instruments were
minted **before** the changed gate ships; the frozen set is run against
the changed gate, and the read ships beside the change. This file is the
temporal record: declarations first, run results appended at execution,
nothing backdated.

## Declaration (minted 2026-08-14, before any gate invocation of this slice)

- Instruments frozen: `sub-bank.jsonl` (12 negative + 6 positive) and
  `longform-negative.jsonl` (6). Authored NWCI — no gate, retrieval, or
  answer text consulted.
- The changed gate = single-string judge (`claim_violation_joint`) +
  C-class containment witness (trigger: judge-supported claims with ≥1
  extracted specific; all specifics absent → downgrade to
  could-not-judge). Witness code: `deep_research/containment.rs`.
- Acceptance shape (declared above, in README.md):
  - baseline (judge alone): most negatives/longforms pass (supported);
  - changed (judge+witness): negatives/longforms downgrade to
    could-not-judge; positives stay supported;
  - the witness never upgrades a verdict.
- The gate change ships (loop audit wired) only with this read recorded.

## Baseline run — judge alone (frozen set)

*Appended 2026-08-14, executed on RuggedFox against the live local daemon
(`http://127.0.0.1:9741/v1`, `Qwen3.6-35B-A3B-MTP-UD-Q6_K`, ctx 8192,
`ShardingPrivacy::LocalOnly`). Driver:
`sovereign-core/tests/adversarial_read.rs` (the production
`claim_violation_joint` + the audit's own splitter); full per-claim rows in
`adversarial-report.json` (this commit). tau read live = 0.9.*

| item set | items | claims | judge-alone passed | judge-alone failed |
|---|---|---|---|---|
| negatives an-01..an-12 | 12 | 12 | **0** | 12 |
| positives ap-01..ap-06 | 6 | 6 | **6** | 0 |
| longform negatives lf-01..lf-06 | 6 | 13 | **0** | 13 |
| total | 24 | 31 | 6 | 25 |

## Changed-gate run — judge + containment witness (frozen set)

*Same driver, same session, same tau. Per-claim transitions: P = passed,
C = could-not-judge, F = failed (N = never-ran — none occurred).*

| item | transition | item | transition |
|---|---|---|---|
| an-01..an-12 | F→F (12) | ap-04 | P→P |
| ap-01 | P→P | ap-05 | **P→C** |
| ap-02 | **P→C** | ap-06 | P→P |
| ap-03 | **P→C** | lf-01..lf-06 | F→F (13 claims) |

Totals: 31 claims; judge-alone supported 6; changed could-not-judge 3;
**downgraded items 3, upgraded items 0**.

## Read

Written 2026-08-14, timestamped beside the run. Verdict rows vs the declared
acceptance shape:

| acceptance row (declared) | observed | verdict |
|---|---|---|
| baseline: most negatives/longforms pass (supported) | all 25 negative/longform claims **failed** judge-alone at tau=0.9 | **refuted** — the shared world-knowledge-bias residual did not reproduce on this frozen set at this tau (the fr6-era residual was measured on the labeled bank's answer+evidence shape; these are single factual claims vs one synthetic window, and the judge catches the mismatch) |
| changed: negatives/longforms downgrade to could-not-judge | they were already failed by the judge alone; the witness never ran on them (trigger = judge-supported only) | not applicable — the trigger population the redesign targeted was empty in this set |
| changed: positives STAY supported (≥1 specific present) | 3/6 stayed; **3/6 downgraded to could-not-judge** | **refuted** — see the I→C finding below |
| the witness never upgrades a verdict | 0 upgrades across all 31 claims | **held** |

**Finding — the witness's dominant failure mode is the I→C shape mismatch,
not content absence.** The three downgraded positives (ap-02, ap-03, ap-05)
all carry specifics that ARE in their windows, but the I-class extraction
reshaped them so the C-class verbatim matcher cannot find them:
`412 megapascals` → `Tensile strength: 412 MPa` (unit normalization +
label prefix), `Ormosia coccinea … dispersed by toucans` → `Species: …` /
`Dispersal agent: …` rows, `750 BCE` → `Date: 750 BCE`. The verbatim
containment matcher then correctly reports absent. This is exactly the
"an extraction error costs a witness *miss*, never a false pass" class the
design priced (gate-redesign.md §3) — now quantified at 50% (3/6) on
verbatim-positive windows.

**Disposition.** The gate change ships with this read recorded: the witness
only downgrades, and a downgraded claim becomes a gap → re-query, never a
false pass (the safety direction is preserved). The measured cost — a
could-not-judge round on genuinely grounded claims — is the priced
I→C miss, now quantified. Recorded follow-up candidates for T1b, each
needing its own pre-registered re-read if adopted: (a) extraction prompt
asking for verbatim spans of the claim rather than reconstructed facts;
(b) a matcher tolerant of label/unit reshapes — must stay C-class, verbatim
containment is the whole point, so this is likely a (a)-side fix.
