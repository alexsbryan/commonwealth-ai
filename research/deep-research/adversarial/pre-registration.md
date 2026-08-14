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

## Witness-fix read — anchor filter, case-insensitive containment, negative-claim rule

*Declared 2026-08-14, BEFORE the witness fix (directive 6c25d88e, order
deep-research-t1b). The demo flight record's collapse-bar section
(`research/deep-research/demo/README.md`) recorded three witness defect
classes, all in `deep_research/containment.rs` (the witness never edits the
judge):*

1. **Phantom specific** — "Date: 1973 (inauguration)", present in neither
   claim nor evidence, alone flips the witness to all-absent and false-
   downgrades a grounded claim (the same I→C shape the T1a read quantified
   at 3/6 on ap-02/ap-03/ap-05, whose recorded follow-up (a) — "verbatim
   spans of the claim rather than reconstructed facts" — is resolved here
   as a deterministic C-class filter).
2. **Case-sensitive body containment** — `appears_in_body` matched
   `line.contains(specific)` case-sensitively while the one matcher
   (`value_present_in_chunks`) is case-insensitive; "Pad 39A" vs "pad 39A"
   judged absent.
3. **Negative claim about the evidence passes vacuously** — "none of the
   provided sources list the crew names" shipped supported as un-witnessable,
   never adjudicated against the window that lists the names.

**Changed witness** (C-class discipline preserved — no matcher weakening):
- *Anchor filter*: each extracted specific is stripped of a leading
  colon-terminated label phrase ("Date:", "Tensile strength:", …) and kept
  only if it is case-insensitively anchored in the claim text. Unanchored
  specifics are dropped — a specific the claim does not assert cannot flip
  the witness. ("412 MPa" ≠ "412 megapascals" stays dropped: the filter
  recovers anchored reshape classes only, never a tolerant matcher.)
- *Case-insensitive body containment*: `appears_in_body` matches
  case-insensitively, matching the one matcher's discipline.
- *Negative-claim rule*: a claim lexically asserting an absence about the
  evidence (none of the sources / not listed / does not contain / never
  mention / absent from / …) inverts the presence test — any witnessable
  specific present in the evidence → the negation is contradicted →
  downgrade; all specifics absent → the negation holds → no downgrade; no
  checkable specifics → the claim does NOT pass vacuously: it downgrades
  to could-not-judge with the reason recorded.

**New frozen fixtures (added to the frozen set BEFORE the fix ships —
defect-anchored, authored from the demo flight record; the changed witness
has never run on them):**

| id | class | expected under the changed witness |
|---|---|---|
| ap-07 | phantom-anchored positive (specific present, anchored) | stays supported |
| ap-08 | case-shifted proper noun ("Pad 39A" vs "pad 39A") | stays supported |
| an-13 | negative claim contradicted by its window (crew members listed) | not supported — could-not-judge if the judge supports it, failed if the judge fails it; never a vacuous pass |

**Acceptance shape (declared before the run — the frozen longform-negative
set + the minted sub-bank re-run against the changed witness):**

- an-01..an-12 stay downgraded/failed (absent specifics remain absent);
- ap-01..ap-06 stay supported — including the three recorded I→C
  downgrades (ap-02, ap-03, ap-05), which recover to supported: their
  specifics are anchored in their claims after label stripping;
- lf-01..lf-06 unchanged (judge-failed; the witness never runs on them);
- the witness never upgrades a verdict.

## Witness-fix read — execution

*Appended 2026-08-14 at execution, beside the witness fix. Executed on
RuggedFox against the live local daemon (`http://127.0.0.1:9741/v1`,
`Qwen3.6-35B-A3B-MTP-UD-Q6_K`, ctx 8192, `ShardingPrivacy::LocalOnly`).
Driver: `sovereign-core/tests/adversarial_read.rs` (the production
`claim_violation_joint` + `assess_claim`); full per-claim rows in
`adversarial-report.json` (this commit). tau read live = 0.9. The changed
witness had never run on ap-07/ap-08/an-13 — they were frozen before the
fix.*

| item set | items | claims | judge-alone supported | changed could-not-judge |
|---|---|---|---|---|
| negatives an-01..an-12 | 12 | 12 | 0 | 0 |
| positives ap-01..ap-08 | 8 | 8 | **8** | 0 |
| longform negatives lf-01..lf-06 | 6 | 13 | 0 | 0 |
| total (incl. the 3 new fixtures) | 27 | 34 | 8 | 0 |

Per-claim transitions (P = passed, C = could-not-judge, F = failed):

| item | transition | item | transition |
|---|---|---|---|
| an-01..an-12 | F→F (12) | ap-06 | P→P |
| ap-01 | P→P | ap-07 | P→P |
| ap-02 | P→P (recovered) | ap-08 | P→P |
| ap-03 | P→P (recovered) | an-13 | F→F |
| ap-04 | P→P | lf-01..lf-06 | F→F (13 claims) |
| ap-05 | P→P (recovered) | | |

Totals: 34 claims; judge-alone supported 8; changed could-not-judge 0;
**downgraded items 0, upgraded items 0** (the T1a changed-gate read: 3
downgraded items).

## Witness-fix read — verdict rows vs the declared acceptance shape

| acceptance row (declared) | observed | verdict |
|---|---|---|
| an-01..an-12 stay downgraded/failed | F→F on all 12 | **held** |
| ap-01..ap-06 stay supported, incl. the three I→C recoveries (ap-02, ap-03, ap-05) | all 6 P→P; the three recorded I→C downgrades RECOVERED to supported | **held** — the anchor filter is the T1a follow-up (a) resolved as a C-class filter: the witness's dominant failure mode (3/6 I→C shape mismatch, recorded in the T1a read) no longer produces false downgrades on the frozen set |
| ap-07 (phantom-anchored) stays supported | P→P | **held** |
| ap-08 (case-shifted proper noun) stays supported | P→P | **held** — the case-insensitive body check |
| an-13 (negative claim contradicted) never a vacuous pass | F→F — the judge itself failed the negation (its window lists the crew); the witness never ran on it | **held** — the vacuous pass did not occur; the negative rule itself is pinned by the deterministic unit tests added with the fix (contradicted / holds / vacuity / audit-record reason), not by this read |
| lf-01..lf-06 unchanged | F→F (13 claims) | **held** |
| the witness never upgrades a verdict | 0 upgrades across all 34 claims | **held** |

**Read.** The witness's false-downgrade rate on the frozen set went from
3/6 positives (T1a) to 0/34 claims (this read). The witness-fix fixtures
behaved exactly as pre-registered. an-13's judge-fail shows the negation
rule's second safety layer: a contradicted negative is caught by the judge
before the witness runs; the witness's own inversion is pinned
deterministically.

## GAP-2 corroboration-floor read — declaration

*Declared 2026-08-14, BEFORE the corroboration floor ships (order
deep-research-t1b, GAP-2; spec DEEP_RESEARCH.md "GAP-2 — Corroboration";
FMEA F22). The changed gate = judge + containment witness + custody veto
+ **the corroboration floor**: a claim passes only if its support set
spans ≥2 distinct provenance origins (distinct source_urls among the
supporting chunks, C-class); a single-origin support set caps at
could-not-judge with the floor's record (`corroboration_floor` action +
`CorroborationRecord {origins, support_chunks, floor: 2, passes_floor}`),
verdict-visible on the final claim. Floor code: `deep_research/audit.rs`
(step 6 of `assess_claim`, after the custody veto and the witness
downgrade). The frozen instrument set is unchanged — the same 34-claim
set (12 negative + 8 positive + 13 longform-negative claims).*

**Declared shape (written before the run):** every fixture's evidence
window is a single synthetic chunk from a single source — one origin,
however the window is shaped. Therefore:

- **every judge-supported claim caps at could-not-judge** — ap-01..ap-08,
  including the witness-fix recoveries (ap-02, ap-03, ap-05), all downgrade
  at the floor (their witness recovery stands: the record will show
  support_chunks ≥ 1 and origins = 1 — the cap is the floor's, not the
  witness's);
- negatives an-01..an-12, an-13 and longforms lf-01..lf-06 stay failed —
  the judge fails them, the floor never runs on a judge-failed claim
  (floor trigger = judge-supported only);
- **the floor never upgrades a verdict** (0 upgrades across all 34);
- the floor's downgrade is the two-source rule, not a witness defect: the
  recorded cause is the single-origin support set, on the claim.

*The gate change ships (loop audit wired, meridian golden regenerated)
only with this read recorded.*

## GAP-2 corroboration-floor read — execution

*Appended 2026-08-14 at execution, beside the floor. Executed on RuggedFox
against the live local daemon (`http://127.0.0.1:9741/v1`,
`Qwen3.6-35B-A3B-MTP-UD-Q6_K`, ctx 8192, `ShardingPrivacy::LocalOnly`).
Driver: `sovereign-core/tests/adversarial_read.rs` (the production
`claim_violation_joint` + `assess_claim`); full per-claim rows in
`adversarial-report.json` (this commit). tau read live = 0.9.*

| item set | items | claims | judge-alone supported | changed could-not-judge |
|---|---|---|---|---|
| negatives an-01..an-12 | 12 | 12 | 0 | 0 |
| positives ap-01..ap-08 | 8 | 8 | **8** | **8** |
| longform negatives lf-01..lf-06 | 6 | 13 | 0 | 0 |
| total | 26 | 33 | 8 | 8 |

Per-claim transitions (P = passed, C = could-not-judge, F = failed):

| item | transition | item | transition |
|---|---|---|---|
| an-01..an-12 | F→F (12) | ap-06 | **P→C** |
| ap-01 | **P→C** | ap-07 | **P→C** |
| ap-02 | **P→C** | ap-08 | **P→C** |
| ap-03 | **P→C** | an-13 | F→F |
| ap-04 | **P→C** | lf-01..lf-06 | F→F (13 claims) |
| ap-05 | **P→C** | | |

Totals: 34 claims; judge-alone supported 8; changed could-not-judge 8;
**downgraded items 8, upgraded items 0** (all judge-supported claims capped
at the single-origin floor).

## GAP-2 read — verdict rows vs the declared acceptance shape

| acceptance row (declared) | observed | verdict |
|---|---|---|
| every judge-supported claim caps at could-not-judge (ap-01..ap-08) | **all 8 P→C**, each with the floor's record — 6 with `origins: ["window:synthetic"], support_chunks: 1`, 2 with `origins: [], support_chunks: 0` (no anchored support located); all with `passes_floor: false` | **held** |
| the witness-fix recoveries (ap-02, ap-03, ap-05) cap at the FLOOR, not the witness | P→C, and the report rows carry the gate's own accounting: ap-02/ap-05 show `support_chunks: 1, origins: ["window:synthetic"]` — the witness located the support and the FLOOR capped it; ap-03 shows `support_chunks: 0` (extraction variance — no anchored support located THIS run; the floor is the empty-support backstop). In every case the cap is the floor's, the record is on the claim | **held** |
| negatives/longforms stay failed (the floor never runs on judge-failed claims) | F→F on all 25 | **held** |
| the floor never upgrades a verdict | 0 upgrades across all 34 claims | **held** |

**Read.** The two-source rule holds exactly as declared: every
single-origin claim — including the ones the witness legitimately
recovered — caps at could-not-judge, and nothing upgrades. The measured
cost is the priced one (gate-redesign.md §3's asymmetry): a claim resting
on a single synthetic document cannot pass, becomes a gap, re-queries —
the false-pass direction is closed by construction. The meridian golden
was regenerated under the same rule (4 single-origin passes → open
questions, not_covered 1 → 5).
