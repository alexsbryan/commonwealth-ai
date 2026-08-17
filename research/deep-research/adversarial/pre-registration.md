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

---

# Declaration — the bank-v1 arms (order deep-research-t1c, minted 2026-08-14)

Declared BEFORE any arm run of this order. Results append below with
*Appended at execution* markers; nothing here is backdated (§18.5/§18.6).

## Protocol

- Measurement on the mock-deck surface (`--backend mock --mock-deck`, the
  t1b CLI surface); drafts DELEGATED to the real local daemon — the model
  genuinely reads the deck bodies and drafts from them, exactly the
  production shape.
- **Model pin:** daemon :9741 — draft `Qwen3.6-35B-A3B-MTP-UD-Q6_K`,
  embed `Qwen3-Embedding-0.6B-Q8_0`; tau 0.9; `--max-rounds 3` (the t1b
  drill surface). No loop/gate code changes: measuring an instrument you
  are editing is not a measurement (§18.4); any needed change is
  OFF-ORDER — escalate.
- **n:** 13 questions — the 12 v0 seeds + the 1 v1 report-class question
  ("How did American cities change across four decades (1980-2024):
  gentrification, inequality, affordability, and displacement — every
  claim cited?"). 12 v0 loop flights + 1 v1 loop flight + 13 one-shot RAG
  comparisons + the 6-flight P5 battery (the t1b fixtures, re-run).
- **Decks:** `research/deep-research/arms/decks/seed-01..12` (single-origin
  estate exemplars; provenance + the single-origin consequence in
  arms/decks/README.md) and `research/deep-research/bank/v1/deck/` (the
  two-origin report-class source deck, frozen sha256
  `e63a14499d849301f3f0bbd00024c178609c5899b97d5b6ec0a6ee5b1e88c5ee`).
  Poisoned dirs run, never edited; bank v0 frozen — run, never edit.
- **Scoring:** the deterministic structured-match checker (bank v0
  semantics, never an LLM judge): a key is CLEARED when the answer names
  the key's subject with the key's figure (or the deck-supported corrected
  figure, arbiter-journaled) AND the run's evidence window supports it;
  partial = gap (all-of rule); the evidence-arbiter rule applies.
  **Honesty is scored separately, never blended into coverage**
  (DEEP_RESEARCH.md P2): the loop's gate verdicts, the corroboration
  floor's caps, and the fabrication rate are reported per run.
- **The K2 conflict clause (v1, deterministic):** a claim asserting the
  superlative ("NYC leads at 0.5469") is NOT covered while 0.57s sit in
  the same report unless the answer names the discrepancy (the deck
  carries both figures — named-source wikipedia-states for 0.5469, the
  exemplar body for the 0.57s/0.56/0.40) or renders could-not-judge;
  conflicting figures across sources must render could-not-judge or a
  named discrepancy, never a synthesized pass. Journaled per occurrence.
- **The two-arm comparator:** the loop's report vs a ONE-SHOT RAG answer
  over the same deck — same daemon, same model, same retrieval; ONLY the
  loop differs. The one-shot arm is a Rust integration test reusing the
  production Deck / MockBackendImpl / `synthesize::draft_round` path —
  zero prompt fork. Both outputs scored by the same deterministic
  checker.
- **Same-commit substitution (named, never silent — §18.3):** the bars
  amendment cites the v1 mint by FROZEN DECK sha256 rather than a commit
  hash, because the amendment lands in the same commit as the mint.
- **Verdicts:** every leg reported four-verdict (§18.2): passed / failed /
  could-not-judge / never-ran. A gate not watched fail is not a gate —
  the P5 verify.sh output and the scorer's fixture checks are part of the
  record.

## Thresholds (proposed at mint, operator-ratified at this order's approval)

| leg | threshold |
|---|---|
| P4 | ≥58 of 72 v0 keys AND ≥12 of 16 v1 keys cleared (K/N per bank) |
| P3 | ≥10 of 13 questions paired-pass: round-2 fetch count < 20% of round-1 with the coverage key not worse (journaled per question) |
| P5 | 6/6 flights pass — fabrication absent from the final report's passed claims AND the control-flow trace identical to the clean twin modulo the wasted round; NO noise band |
| R-12 | gap-set strict shrinking on ≥10 of 12 v0 seeds (v1 convergence journaled, not gated) |
| Two-arm lift | pooled attribution density: loop ≥ one-shot + 0.10; v1-question density: loop ≥ one-shot + 0.15; honesty (fabrication rate) not worse |

Attribution density = the fraction of numeric claims in the output that
trace to deck sources (deterministic checker over the output text vs the
run's evidence window). Lift decided per metric; each leg reported
four-verdict.

---

# Ceiling probe — declaration (order deep-research-t1d, declared 2026-08-14)

Declared BEFORE the probe runs. The probe is the order's FIRST mandated
action ("probe the ceiling FIRST, then fix acquisition"): it answers with
a number — "how many of the 72 v0 + 16 v1 keys are reachable if
acquisition were perfect" — and the v0 number vs the P4 bar (58) decides
whether the acquisition fixes proceed (`>= 58`) or the order escalates
with the probe evidence and stops fix work (`< 58`).

## Method (the journaled definition of "reachable")

- **Perfect acquisition** = the loop's window holds EVERY deck body, full
  text. The window content cap is 12k chars per chunk
  (fetch.rs CHUNK_CONTENT_CAP); every body in both frozen banks measures
  < 12k bytes (checked at declaration: max = governing.md 7203), so the
  cap is immaterial to the ceiling — noted, not approximated.
- **Content reachability** (the coverage ceiling — the number that gates):
  the scorer's evidence side, applied over the full-deck window, using
  the scorer's own extractors with NO reimplemented threshold (§10.6):
  `probe-ceiling.py` imports `figures_of` / `figure_present` /
  `subjects_of` / `parse_v0_keys` / `parse_v1_keys` / `V1_CORRECTIONS`
  from `score-arms.py` and applies `score_keys`'s window-side rules
  verbatim:
  - journaled cannot-clear keys (v1 K9): unreachable;
  - corrected figure keys (K2/K7 require-forms): every required figure
    present in the concatenated bodies AND >= 1 required subject present
    (a subject absent from the deck is not nameable by any acquisition);
  - corrected figureless keys (K4): every required subject present,
    dot-normalized (the scorer's own normalization for corrected
    figureless keys);
  - base figureless keys (the v0 causal links): >= 2 distinct subjects
    present (the scorer's >= 2 rule).
- **Floor reachability** (the honesty ceiling — journaled separately,
  never blended into coverage, P2): the corroboration floor (audit.rs
  `CORROBORATION_FLOOR = 2`, C-class distinct source_urls) run directly
  over the full deck. A key is floor-reachable iff >= 2 distinct deck
  origins (hit URLs) carry ANY of the key's required content — the
  OPTIMISTIC bound (the draft must actually cite both origins). A key
  whose content lives on a single origin is capped by the floor forever,
  however acquisition runs. Known structural facts at declaration:
  every v0 deck is single-origin (arms/decks/README.md — estate
  exemplars by design), so the v0 floor ceiling is 0/72 by construction;
  v1 K5 and K9 are journaled exemplar-only (seeds.md arbiter journal) —
  expected single-origin, floor-capped, reported.
- **Deterministic, no model, no network.** The probe writes
  `arms/ceiling-probe.json` (per-key rows) beside the script.

## Declared decision rule

v0 content ceiling >= 58 → PROCEED to the three acquisition fixes (each
red-first). v0 content ceiling < 58 → ESCALATE to the seat with the probe
evidence and STOP all fix work (the bar/deck re-cut is the operator's).

## Ceiling probe — execution

*Appended at execution, 2026-08-14. Probe: `arms/probe-ceiling.py`
(deterministic, no model, no network — imports the scorer's own
extractors, §10.6). Per-key rows in `arms/ceiling-probe.json`.*

| bank | content ceiling (the gate number) | floor ceiling (honesty side) |
|---|---|---|
| v0 (12 seeds × 6 keys = 72) | **72/72** | **0/72** — every v0 deck is single-origin (estate exemplars by design, arms/decks/README.md); the corroboration floor caps every v0 claim by construction, however acquisition runs. This is the structural reason R-12 on v0 cannot reach strict-shrink-to-zero with the floor on — journaled, never hidden |
| v1 (16 keys) | **13/16** | **15/16** (optimistic bound — >= 2 origins carry some of the key's content; the real floor verdict depends on what each claim cites) |

**Decision: v0 content ceiling 72 >= 58 → PROCEED to the acquisition
fixes** (each red-first, per the order).

**Unreachable keys, named (each journaled, none re-cut):**

- v1 K3 (80/20 ratio: 7.87:1 / 7.81:1 / $172,476 / $22,095): the deck
  carries every figure VERBATIM EXCEPT the ':1' canonical form — the
  bodies spell "income inequality ratio of 7.87" / "at 7.81"
  (smartasset.md, source-report.md), and the frozen scorer's
  `figure_regex` for the ':1' family requires `7.87 : 1` or `7.87 to 1`
  in the window. Under the frozen scorer semantics the evidence side
  can never match, whatever acquisition does. The figures themselves
  are deck-present — this is a canonical-form artifact of the FROZEN
  scorer against the FROZEN deck, not an acquisition gap. Noted for the
  operator; the bar re-cut is the operator's, never this order's.
- v1 K9 (48 of 50): journaled cannot-clear at bank mint (arbiter
  journal — "no named source carries 48 of 50"; the exemplar's own
  prose blanks the count). The probe records the journal's verdict.
- v1 K13 (poverty −0.7pp / +6.7pp): the governing body spells the
  figures "percentage-point change -0.7 … vs +6.7" (unit word precedes
  the number); the frozen scorer's 'pp' canonical requires the unit
  AFTER the figure ("0.7pp" / "0.7 percentage points"). Same
  canonical-form artifact class as K3 — deck-present, scorer-
  unreachable. Noted; not re-cut.

**Consequence for the v1 P4 bar (>= 12 of 16):** the ceiling is 13/16 —
the bar stays reachable (with one key of slack), and the re-measurement
journals K3/K13/K9 per key rather than folding them into "failed". The
v0 P4 bar (>= 58/72) has a ceiling of 72/72 — the acquisition fixes are
the only thing between the loop and it.

## Acquisition fixes — declaration (order deep-research-t1d, declared 2026-08-14)

The three fixes below land red-first (fails at HEAD, then green),
per the order. The red shapes were each recorded BEFORE the fix landed
(§18.1 — a gate watched fail is a gate):

- **Fix 1 — dedup**: "a round-2 fetch of an already-fetched URL is
  refused" (no second fetch, no spend, refusal recorded). Red at HEAD
  was the t1c-observed shape: the same URL admitted twice and fetched
  twice (fetch.rs custody tests + demo flight). Fix: fetch_round
  refuses URLs in `already_fetched` (round-scoped from
  Controller.fetched_sources) BEFORE the decider — structurally, with
  `dedup_refused` recorded on the window ICD. Unit test:
  `already_fetched_url_is_refused` (within-round and cross-round
  parts).
- **Fix 2 — breadth**: "round-1 queries cover every deck hit for the
  v1 question". Red at HEAD (measured in t1c, fetch-list-1.json of
  dr-1786748480): round 1 asked ONLY the question — the empty-window
  gap's query — so 4 of 11 v1 hits reached the window. Fix: the plan's
  acquisition frontier — `plan_subquestions` on the port trait
  (deterministic clause-split default, mock follows its draft surface,
  CLI delegates a constrained decomposition draft, FRONTIER_MAX = 12
  one decider) — recorded as plan.json `queries_preplanned` and joined
  to the round-1 query set only (rounds 2+ stay gap-targeted). Red
  demonstrated in-tree: frontier join disabled (the pre-fix shape) →
  test failed with 0 frontier queries vs 8; fix restored → green.
  Unit test: `round1_queries_cover_every_deck_hit`.
- **Fix 3 — second-origin**: "when the floor caps a claim, the next
  round targets the capped claim's missing origin". Red at HEAD
  (watched, this run): the gap query is the claim's first-140-chars
  prose — a figure beyond the cut is never queried (the t1c R-12
  structural deadlock: 0/12 on v0 single-origin decks). Fix: the Gap
  ICD carries the floor's CorroborationRecord; the loop's ONE
  gap→query decider (`gap_query_for`) forms a FACT query (the claim's
  figure tokens + content words, C-class deterministic, ~200-char cap)
  exactly when the record fails the floor; the record rides the formed
  query into the fetch list (self-describing artifacts). Unit tests:
  `floor_capped_gap_query_targets_the_missing_origin_fact`,
  `floor_record_rides_the_formed_query`.

## Acquisition fixes — execution

*Appended at execution, 2026-08-14.*

| fix | red watched (pre-fix) | green after fix | notes |
|---|---|---|---|
| 1 dedup | pass 0 fail 1 at HEAD (same URL admitted twice, fetched twice) | pass 1 fail 0 | refusal before the decider; spend zero; `dedup_refused` on the window ICD |
| 2 breadth | fail 1 — pre-fix shape (frontier join disabled): "round 1 must carry the full acquisition frontier as queries", left 0 right 8 | pass 1 fail 0 | plan.json records queries_preplanned + source "plan-subquestions"; all 8 deck hits covered by round-1 queries; red evidence in target/sovereign-test/latest/cargo.raw.log |
| 3 second-origin | fail 1 at HEAD — gap query was the 140-char prose cut: "A long background clause that carries no load-bearing figure. A long background clause that carries no load-bearing figure. A long backgroun" (no 0.55) | pass 2 fail 0 | fact query carries figures + content words; gap + formed query carry the floor's record; non-capped claims keep the prose template |

deep_research module after all three: 64/64 green (was 62; +1 fix-3
unit, +1 form_queries record test).

## Full battery re-measurement — declaration (order deep-research-t1d, declared 2026-08-14 BEFORE the runs)

- **What re-runs**: the full dr-local-loop battery against the FROZEN
  banks — same decks, same scorer, same model pin (daemon :9741 —
  Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed,
  tau 0.9, max-rounds 3), same driver (`arms/run-arms.sh` — the 13
  flights + the one-shot comparator through `oneshot_rag.rs`), same
  scoring (`arms/score-arms.py`, C-class, unchanged), plus the P5
  6-flight honesty drill (`demo/p5/run-flights.sh`).
- **Declared protocol change — the acquisition budget rises from the
  CLI default 4/4 to 12/12** (`--search 12 --fetch 12`), with reason:
  the t1c run exhausted the v1 round-1 budget (4/4) before the
  breadth fix's whole frontier could be asked; the three acquisition
  fixes are designed against the frozen banks, and a round-1 frontier
  of ~8-11 sub-questions needs ~12 searches. The change is made once,
  pre-registered, for BOTH the v0 and v1 legs (same protocol). The
  one-shot comparator leg is budget-agnostic by design (full-window
  draft) and unchanged. Env overrides SEARCH_ALLOWANCE /
  FETCH_ALLOWANCE reproduce the t1c 4/4 protocol verbatim.
- **Archival**: the t1c loop flight recorders were moved to
  `arms/runs/loop-t1c/` before the re-measurement (run-arms.sh writes
  `loop/<id>/`); the t1c scored numbers remain in
  `arms/score-report.json`. Re-measurement output:
  `arms/score-report-t1d.json`.
- **Honesty floor**: P5 stays 6/6 (zero ungrounded load-bearing in any
  arm) and zero-ungrounded-load-bearing stays the P4 gate's honesty
  side — never traded for coverage.
- **Declared decision rule**: the transition is rewritten from the
  measured outcome — P4-v0 >= 58/72, P4-v1 >= 12/16, R-12 >= 12/12
  convergence, P3 >= 1 (probe sanity), and the two-arm lift (loop
  coverage - one-shot coverage) — with four verdicts per leg
  (passed / failed / could-not-judge / never-ran), never silent.

## Full battery re-measurement — execution

*Appended at execution, 2026-08-14.*

- **Runs**: 13/13 loop flights (`arms/run-arms.sh`, seeds v0 seed-01..seed-12
  + v1, each a fresh `arms/runs/loop/dr-<epoch>/` against the FROZEN banks)
  exited 0; the one-shot comparator (full-window draft, 126s) exited 0.
  Protocol as declared: budget 12/12 (`--search 12 --fetch 12`), model pin
  daemon :9741 (Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0
  embed, tau 0.9, max-rounds 3). Scored by `arms/score-arms.py` →
  `arms/score-report-t1d.json` (pre-fix evidence preserved at
  `arms/score-report-t1d-raw.json`).

- **Verdicts per the declared decision rule** (four verdicts, never silent):

  | leg | bar | measured | verdict |
  |---|---|---|---|
  | P4-v0 | >= 58/72 | 49/72 | failed (deltas: seed-03 -1, seed-08 -2, seed-09 +1, seed-11 -1 — draft variance on single-origin decks) |
  | P4-v1 | >= 12/16 | 2/16 | failed (one-shot comparator: 5/16 — the loop rounds spent on acquisition, draft sub-questions never surfaced the figure tokens) |
  | P3 | >= 10/13 | 12/13 | passed (dedup live: round-2 fetches 0 < 20% of round-1; the one v1 fail is probe-sanity shape on the v1 deck, not dedup) |
  | R-12 | >= 10/12 | 0/12 | failed (structural: v0 decks are single-origin, the floor is never weakened, so the audit adds floor caps and gap sets only grow — same shape as t1c; v1 journaled 1 -> 26, not gated) |
  | two-arm lift (pooled) | loop >= one-shot + 0.10 | 1.0 vs 0.976 | failed (both arms trace every numeric claim; lift is thin because one-shot is already dense) |
  | two-arm lift (v1) | loop >= one-shot + 0.15 | 1.0 vs 1.0 | failed |
  | honesty not worse | loop ungrounded <= one-shot | loop 0.0 vs one-shot 0.024 | passed |

  Every leg measured; no leg could-not-judge or never-ran. Per-question
  coverage table (loop_covered / oneshot_covered): seed-01 3/6-3/6, seed-02
  5/6-5/6, seed-03 4/7-5/7, seed-04 4/6-4/6, seed-05 5/6-5/6, seed-06
  6/6-6/6, seed-07 5/6-4/6, seed-08 2/5-3/5, seed-09 3/6-2/6, seed-10
  4/6-4/6, seed-11 3/6-4/6, seed-12 5/6-5/6, v1 2/16-5/16.

- **Scorer instrument defect journal (before -> after, both directions,
  per §18.6)**: the first scoring pass flagged an honesty failure (loop
  ungrounded 0.059 vs one-shot 0.024, seed-08 loop density 0.0, seed-06
  one-shot 0.857). Direct evidence showed BOTH flagged non-tracing claims
  are scorer defects, not loop dishonesty:
  1. `numeric_claims` counted the report header as a claim — the title line
     and the run-metadata line carry the 10-digit run id, which tokenized
     as an ungrounded numeric claim (seed-08's report has zero real
     numeric claims, so 0/1 -> density 0.0). Fixed: header sentences
     (title, `- run:` metadata) excluded from claims.
  2. `NUMERIC_TOKEN`'s dollar branch truncated "$500M" to "$500" (unit
     dropped), so the seed-06 one-shot claim "Delta Airlines incurred
     approximately $500M..." failed the trace check even though the
     window contains "$500M" verbatim (verified before fixing:
     `figure_present(("500","m"), window)` True, `("500",None)` False).
     Fixed: dollar branch carries the unit suffix, matching `FIGURE_RE`.
  After the fix: honesty leg loop 0.0 <= one-shot 0.024 -> passed (was
  falsely failed); the coverage verdicts (P4, P3, R-12) are unchanged by
  the fix — the instrument correction touches only the density/lift/
  honesty legs; thresholds, keys, decks, and protocols untouched (no bar
  re-cut). Loop density 1.0 -> the loop arm's true ungrounded is 0.0.

- **v1 flight mechanics (dr-1786754967, the report-class question)**:
  round-1 carried 5 queries (1 gap-template + 4 plan-subquestion
  frontier; the frontier join is live — t1c had 1). 6/11 deck hits
  covered by round-1 queries; the 5 figure-specific hits (Gini 0.5469
  NYC, New Orleans income-inequality ratio, white-share bachelor, mfg
  jobs, Case-Shiller 325.78 mobility) unreachable because their tokens
  never entered the evidence. K-cut admitted 4/round (governing,
  constructioncoverage, stanford, terry-uga); wiki-inequality rank 5 and
  brookings rank 7 cut. Round-2: dedup refused all 4 round-1 URLs
  (`dedup_refused` on the window ICD — fix 1 live); 16 formed queries,
  15 floor-capped with the floor's CorroborationRecord riding (fix 3
  live). Budget 12 searches exhausted before a round-3 fetch-list. The
  report is honest: 1 refuted claim, all else could-not-judge floor-
  capped, a "searched but absent" section naming what was not found.

- **Daemon contention note**: during the battery the reindexer-fix
  worker's wl-judge flood (13,204 judge calls over 17.5h, ~one per 0.6s
  on the daemon's primary slot) contended with the flight drafts —
  flights ran 2-3x slower than a quiet daemon. This is a speed effect,
  not a wedge: every flight completed exit 0 under the same protocol,
  so the measurements stand as executed. Nothing was re-run after the
  flood subsided; the numbers above are the runs as they happened.

## T1.7 query formation — declaration (order deep-research-t1e, declared 2026-08-14 BEFORE the runs)

*Declared before any t1e battery flight; the probe flight (probe-t1e) ran after this section was written.*

**Hypothesis.** The t1d v1 flight measured the cap: round-1 queries (question + 4 plan sub-questions) covered 6/11 deck hits; the 5 figure-specific hits (wikipedia-states Gini 0.5469, smartasset 7.87:1, brookings 95/20, pew white-share, cooper-center mfg) were unreachable because the figure tokens never entered the acquisition, and the K-cut admitted by insertion order at all-0.9 ties. The fix (three mechanisms, all deterministic or generic-shape):

1. **R1 prompt shape (generic)**: `plan_subquestions` now asks the draft to name the specific measure/statistic each sub-question implies (an index, a ratio, a share, a rate, a count, a median, a price, a percentage change) and the entities (cities, years). No bank vocabulary — the prompt names shapes, never measures.
2. **R1/R4 fold-in (deterministic)**: `figure_specifiers(question)` (the question's own digit runs + its measure-family words, whole-word, case-insensitive) is recorded on the plan artifact and folded into any sub-question (R1) or gap query (R4) that carries no specifier — `sub-question (1980, 2024, income)`. The plan artifact's sub-questions carry figure specifiers for a question whose own text implies figures, whatever the draft returned.
3. **R5 admission tie-break (deterministic)**: `triage_hits` sorts score-first, then figure-bearing-ness (the hit's own title/snippet carries a digit run), then insertion order. The K-cut cannot silently exclude the hits the figures live in. The triage outcome records the rule (`score-then-figure-bearing`).

**Red-first evidence (watched fail at the pre-fix shape, 2026-08-14, in-tree disable + restore)**:

| test | pre-fix shape | watched failure |
|---|---|---|
| `plan_subquestions_carry_figure_specifiers` (SHAPE: the plan's sub-questions carry a digit or measure word for a figure-implying question; fixture question "How did income inequality and housing affordability evolve across US cities from 1980 to 2024?" — never bank-derived) | fold-in off | `the plan's sub-question must carry a figure specifier (a digit or a measure word): "What were the primary drivers of the change in American cities?"` |
| `gap_query_folds_in_question_specifiers` (R4) | fold-in off | `the figure-less claim's query must carry the question's specifiers` |
| `triage_favors_figure_bearing_hits` (R5) | score-only sort | `the figure-bearing hit must be admitted into the code-set K, not cut by insertion order` — first fixture attempt saturated the decider (ids "h1".. leaked digits into titles, making every hit figure-bearing); fixture corrected to digit-free titles and the red re-watched cleanly before the green |

All four new tests green after restore (70/70 deep_research module tests).

**Measurement protocol (identical to t1d; frozen banks, run never edited)**:
- `arms/run-arms.sh` (13 loop flights, v0 seed-01..seed-12 + v1) + one-shot comparator; budget 12/12 (`--search 12 --fetch 12`), `--max-rounds 3`, model pin daemon :9741 (Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9), `--backend mock --mock-deck <frozen deck>`. Scored by `arms/score-arms.py` → `arms/score-report-t1e.json` (raw preserved).
- **Primary metric (the cap's measurement — frontier figure-specifier presence)**: for each flight, the plan artifact (`plan.json` + re-plans) must show (a) non-empty `figure_specifiers` when the question's own text implies figures, and (b) the Python scorer's independent re-derivation — every plan sub-question text carries a digit run or a measure-family word. The scorer's lexicon is the same 31-word family list (index, ratio, share, rate, percent, percentage, median, average, mean, count, number, price, income, earnings, wage, salary, employment, jobs, population, mobility, cost, rent, poverty, wealth, proportion, statistic, metric, estimate, amount, total, level) — shapes, not bank measures. A sub-question the draft itself named a measure for counts as carrying (never overwritten).
- **Decision rule**: the fix is measured by the presence metric rising to 13/13 flights with a figure-implying question carrying specifiers in the plan artifact (v1's 1980-2024 is the figure-implying case; v0 seeds with no figures in their own text are exempt — a question that implies no figures has no specifiers to carry), PLUS the t1d legs re-measured on the same frozen banks and the same protocols (P4-v0 >= 58/72, P4-v1 >= 12/16, P3 >= 10/13, R-12 >= 10/12, two-arm lift, honesty-not-worse with loop ungrounded <= one-shot). Honesty is never traded: zero ungrounded claims in any arm, floor/witness unchanged.
- **Daemon contention**: if flights run 2-3x slow under peer load, that is journaled, not re-run (same rule as t1d).

## T1.7 query formation — execution

*Appended at execution, 2026-08-14.*

- **Runs**: 13/13 loop flights (`arms/run-arms.sh`, seeds v0 seed-01..seed-12
  + v1, each a fresh `arms/runs/loop/dr-<epoch>/` against the FROZEN banks)
  exited 0; the one-shot comparator (full-window draft, ~126s) exited 0.
  Protocol as declared: budget 12/12, max-rounds 3, model pin daemon :9741
  (Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed,
  tau 0.9). Scored by `arms/score-arms.py` →
  `arms/score-report-t1e.json`. The probe flight (`probe-t1e/dr-1786759798`,
  fresh binary) validated the artifact shape first: plan `figure_specifiers`
  ["1980", "2024"], 4/4 plan sub-questions carrying, triage
  `admission_rule = score-then-figure-bearing` on every fetch list, honest
  report (done-partial, verdict-stamped). A first probe attempt ran the
  STALE pre-rebuild binary (plan.json without `figure_specifiers`) —
  journaled dead-end, re-run on the fresh binary.

- **Verdicts per the declared decision rule** (four verdicts, never silent):

  | leg | bar | measured | verdict |
  |---|---|---|---|
  | T1.7 plan presence (the order's primary metric) | all figure-implying flights carry | 12/12 scoped flights (v1 + 11 v0 seeds; seed-08's question implies no figures — exempt by the declared rule) | passed |
  | P4-v0 | >= 58/72 | 52/72 | failed (per-flight: 3,5,5,5,5,6,3,4,3,4,4,5 of 6 — draft variance on single-origin decks; every uncovered key journaled in the per-key rows) |
  | P4-v1 (loop) | >= 12/16 | 3/16 loop, 7/16 one-shot | failed (the deck's SPECIFIC values — Gini 0.5469, Case-Shiller 325.78 — can never be folded into acquisition without bank leakage: the fold-in carries the question's own tokens by design; the residual cap is the deck's, not acquisition's) |
  | P3 | >= 10/13 | 13/13 | passed |
  | R-12 | >= 10/12 | 0/12 v0 seeds | failed (structural, unchanged from t1d: single-origin decks + floor never weakened -> gap sets only grow) |
  | two-arm lift (pooled) | loop >= one-shot + 0.10 | 0.883 vs 1.0 | failed (one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced — the floor's honest disclosure, not silent numbers) |
  | two-arm lift (v1) | loop >= one-shot + 0.15 | 0.731 vs 1.0 | failed |
  | honesty not worse | loop ungrounded <= one-shot | loop 0.117 vs one-shot 0.0 | failed BY LETTER, passed by the load-bearing property — journaled below, never silent |

  Per-question coverage (loop_covered / oneshot_covered): seed-01 3/6-3/6,
  seed-02 5/6-5/6, seed-03 5/7-5/7, seed-04 5/6-4/6, seed-05 5/6-5/6,
  seed-06 6/6-6/6, seed-07 3/6-5/6, seed-08 4/5-4/5, seed-09 3/6-4/6,
  seed-10 4/6-4/6, seed-11 4/6-4/6, seed-12 5/6-6/6, v1 3/16-7/16.

- **Scorer instrument defect journal #2 (before -> after, both directions,
  per §18.6) — the t1e battery's honesty measurement**: the first scoring
  pass produced numbers that direct evidence showed were instrument
  artifacts, not loop behavior. Three defects, each fixed and re-verified
  with the t1d epoch re-derived under the SAME final instrument:

  1. **Report collapsing (the renderer's contract)**: `sentences()` split
     flat text on ". " + capital; the renderer emits ONE claim per bullet
     line ending in a citation bracket / verdict stamp / em-dash, so no
     boundary existed and each report collapsed into one '#'-headed
     sentence that the header guard skipped. Before: t1e v1 loop density
     NEVER-RAN (0 numeric claims) on a figure-rich report, and t1d's v1
     "density 1.0" was the searched-but-absent section's quoted query
     years parsing as the report's only claim. Fixed: split per line,
     then SENT_SPLIT within lines; `numeric_claims()` cuts at the
     "## Searched but absent" marker (the compass's named absence is a
     section, not claims). After: t1e v1 density 0.731 on 26 real
     claims.
  2. **Case-sensitive unit capture**: `NUMERIC_TOKEN` carried the unit
     suffix in the pattern (the t1d journal's fix) but NOT the
     re.IGNORECASE flag `FIGURE_RE` has — "$500M" tokenized as "$500",
     canon ("500", None), and `\b500\b` cannot match "$500M" verbatim in
     the window. Before: the seed-06 "$500M" claim untraced in BOTH arms
     and BOTH epochs despite `figure_present(("500","m"), window) ==
     True` — the t1d journal's claimed fix never actually flipped that
     trace. After: ("500","m") -> traced (flips are one-directional:
     a canonical unit only adds matches, never removes). t1d one-shot
     "ungrounded 0.024" was this defect.
  3. **List markers as claims**: "1." from a numbered bullet line
     tokenized as "1." and canonized as a decimal ("1.", None) — a bare
     trailing dot is a list marker, not a figure. Before: seed-12 (both
     epochs) scored 0.375 on 3 real claims + 5 marker claims. After:
     trailing dot stripped, "1" fails the bare-minimum rule -> filtered;
     seed-12 density 1.0.

  Coverage verdicts (P4-v0, P4-v1, P3, R-12, T1.7) are UNCHANGED by the
  instrument fixes: `score_keys` reads `figures_of` (FIGURE_RE) and
  `subjects_of`, neither of which the fixes touch; only the density /
  lift / honesty legs moved. Final honesty numbers under the fixed
  instrument, both epochs like-with-like:

  | epoch | pooled loop density | loop ungrounded | one-shot ungrounded |
  |---|---|---|---|
  | t1d (fixed instrument re-derivation) | 75/149 = 0.503 | 0.497 | ~0.0 (the t1d one-shot "0.024" was defect #2) |
  | t1e (this battery) | 53/60 = 0.883 | 0.117 | 0.0 |

  The t1d "loop ungrounded 0.0" pass was instrument-blind (defect #1:
  most t1d reports scored never-ran or one garbage claim); under the same
  working instrument the t1e battery's loop ungrounded (0.117) is 0.38
  LOWER than the t1d baseline (0.497) — honesty improved, never traded.

- **The load-bearing honesty property (checked, not assumed)**: every
  untraced numeric sentence in every artifact (26 loop flights + 13
  one-shot arms, both epochs) was enumerated. ZERO sit in the
  [passed]/Findings position — no untraced number is presented as fact in
  any arm. All untraced are verdict-stamped: `[failed]` lines (e.g. the
  t1d seed-05 draft's self-correcting COT leak — every fabricated date
  the floor flagged) and `[could-not-judge]` floor-capped open questions
  (v1's fragment years "197 s", "198", "200" — the draft's mid-run digit
  degradation on single-origin claims — and "-0.7%" restating the deck's
  "-0.7 percentage points": the value is deck-present, the unit
  restated, frozen canon keeps pp != %). The letter bar "loop ungrounded
  <= one-shot" fails on BOTH epochs under the working instrument (the
  loop's open-questions section is the floor's honest disclosure; the
  ungated one-shot has no such section) — reported, never silently
  substituted; done-when (e) is carried by the property above, which
  held in every arm.

- **v1 flight mechanics (dr-1786760406)**: plan artifact records
  figure_specifiers ["1980", "2024"] (the question's own digit runs);
  the 4 plan sub-questions each carry a specifier — the draft itself
  named measures ("percentage change in median home prices and
  rent-to-income ratios", "counts of census tracts", "Gini coefficient
  or income share ratio", "displacement rates ... percentages") — never
  overwritten (Python re-derivation). Round-1: 5 searches (4
  sub-question queries + 1 gap template), 3 fetched with
  admission_rule=score-then-figure-bearing; round-2: 7 searches, dedup
  refused all round-1 URLs (0 fetched), gaps 1 -> 22 (floor's
  CorroborationRecord riding); round-3: 3 fetched, gaps 22 -> 36;
  budget exhausted. Report honest: 2 passed findings, refuted claims
  flagged and never removed, 22 could-not-judge floor-capped open
  questions with reasons, "searched but absent" section. 3/16 keys
  covered: the deck's specific values (Gini 0.5469, 325.78, 4.3pp
  white share) were fetched but are unreachable under the frozen
  scorer's canonical forms — the acquisition cap is closed for
  everything the question itself implies.

- **Daemon contention note**: the battery ran under the reindexer-fix
  worker's wl-judge flood on the daemon's primary slot (13k+ judge calls
  over the window, per the t1d note) — flights ran 2-3x slower than a
  quiet daemon. Speed effect, not a wedge: every flight exited 0 under
  the same protocol; nothing re-run after the flood subsided.
## T1.9 realistic mock retrieval — declaration (order deep-research-t1f, declared 2026-08-15 BEFORE any code change or re-measure)

**The instrument gap, journaled from t1e.** The mock's search leg matches
the loop's queries to deck docs by EXACT-VALUE lookup: a hit is returned
only when the query contains one of the deck's curated match tokens
(case-insensitive substring, OR-matched). Those tokens carry the bank's
exact figures (Gini 0.5469, Case-Shiller 325.78, the 95/20 9.3) — so a
query must already NAME the bank's figures to retrieve the docs the
figures live in, and an honest loop cannot do that by design (bank
vocabulary never enters a prompt). t1e measured the cap: P4-v1 3/16
loop vs 7/16 one-shot, the residual gap journaled as "the deck's SPECIFIC
values ... unreachable under the frozen scorer". Real search retrieves
documents by TERM relevance: "NYC Gini coefficient" hits the document
containing 0.5469 without the loop ever knowing the value. This is an
INSTRUMENT change — the t1c/t1d/t1e numbers were measured under the
exact-value instrument and are stated as old-instrument numbers,
never mixed with the re-measure (§18.6: this declaration precedes any
re-measure run).

**Pre-registered retrieval semantics (the changed instrument):**

- **Matching model — term overlap, not token substring.** Query and
  document are tokenized by ONE tokenizer (lowercase; split on
  non-alphanumeric; empty tokens dropped; query terms deduped — a
  repeated word scores once). A hit's indexed document is its full
  declared surface: match tokens + title + snippet + body file. The
  term index is built over those texts at deck load (the deck already
  carries the bodies — the harvest; a missing body refuses at load, so
  the index is total over the deck's hits). A hit is retrieved iff at
  least one query term is in its term set (relevance > 0).
- **Ranking — relevance first, deck prior breaks ties.** `relevance` =
  the number of distinct query terms present in the hit's term set.
  Hits rank: relevance desc, then the deck's declared `score` desc (the
  deck's prior — F25's 0.9-vs-0.1 fixture — breaks retrieval ties),
  then deck insertion order. The returned hit score IS the relevance
  (the loop's triage ranks by it; the deck prior never overrides a
  relevance difference). Zero-overlap queries return Ok(empty) — the
  F1/F28 record, unchanged.
- **Deck format contract — unchanged, additive none.** The term index
  derives from the existing contract (match tokens + title + snippet +
  body files); no new deck field, and the frozen banks are read, never
  edited. A decimal figure tokenizes per the one tokenizer (0.5469 →
  terms "0", "5469") — the same split a punctuation-splitting analyzer
  makes; ranking absorbs it.
- **Red-first shape (watched before the fix, §18.1):** a query for a
  CONCEPT (no figure, no match-token substring) retrieves the deck
  document whose BODY carries the value. The exact-match instrument
  returns zero hits for that query — the red; the term-ranked
  instrument retrieves the value-bearing document — the green. The
  fixture mirrors the v1 wikipedia-states shape (body "New York City
  (Gini index 0.5469)") with deliberately value-less, concept-free
  match tokens.
- **Glassbox:** the mock logs the retrieval decider it ran
  (`term-ranked`, one decider, one name) on every search; the run
  artifacts (fetch lists) carry the relevance scores the loop ranked
  by — the t1e-era fetch lists show all-0.9 ties, the re-measure's
  show the term-relevance ranks.
- **§11/§19 survey (what was checked before building):** the house's
  search-gym precedent — `sovereign/bench/search-gym/` (the lane's
  on-disk fixture shape gym.rs already composes, per the module header)
  and `sovereign/bench/CI_GATE_HANDOFF.md` — is a tool-judiciousness
  bench, not a retrieval engine: it cannot serve the loop's search
  leg. The loop's estate search (`estate_search`) is decked-empty in
  v1 (F13/F16) — the corpus-search surface is rung 2 of the operator's
  named program (this order names it, never builds it). The real
  retrieval engine (corpus-engine) lives behind the estate and is
  unreachable from the mock leg by design. Term-ranked retrieval over
  the deck's own bodies is therefore the minimal faithful shape for
  the deck surface, not a parallel surface.

**Re-measure protocol (identical to t1e; frozen banks, run never
edited):** `arms/run-arms.sh` — 13 loop flights (12 v0 seeds + the v1
report-class question), budget 12/12 (`--search 12 --fetch 12`),
`--max-rounds 3`, model pin daemon :9741 (Qwen3.6-35B-A3B-MTP-UD-Q6_K
draft, Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9), `--backend mock
--mock-deck <frozen deck>` — plus the one-shot comparator
(`tests/oneshot_rag.rs`, same retrieval = full-deck window, only the
loop differs) and the P5 6-flight drill (`demo/p5/run-flights.sh` +
`verify.sh`). Scored by `arms/score-arms.py` (unchanged, C-class) →
`arms/score-report-t1f.json` (raw preserved).

**Decision rule (four verdicts per leg, never silent):** P4-v0
≥ 58/72 (the bar; the v0 target — old-instrument 52/72 at t1e);
P4-v1 ≥ 12/16 reported (loop arm; one-shot reported beside — the
old-instrument numbers: 3/16 loop, 7/16 one-shot); P3 ≥ 10/13 not
regressed (t1e 13/13); R-12 ≥ 10/12 reported (the structural
single-origin shape is expected unchanged — the floor is never
weakened and this change is the retrieval instrument, not the floor);
P5 6/6, no noise band; honesty — zero untraced figures in [passed]
position in ANY arm (the load-bearing property, checked not assumed,
never traded for coverage); two-arm lift reported (pooled ≥ +0.10,
v1 ≥ +0.15).

**DEMO-5** (`research/deep-research/demo/demo5/`): the v1 report
rendered by the loop searching the way real search works — values
coming from the DOCUMENTS, queries staying honest — with the
re-measured bars beside it. DEMO-5 IS the strong demo if P4 clears
the floor with zero ungrounded; otherwise it is the evidence that the
instrument hypothesis was wrong and the re-cut moves to the bank's key
design (the operator's call either way).

## T1.9 realistic mock retrieval — execution

*Appended at execution, 2026-08-15.*

- **Runs**: 13/13 loop flights (`arms/run-arms.sh`, epochs
  dr-1786847168..dr-1786847802 — the battery's own, one per seed under
  `arms/runs/loop/*/`) exited 0, each round-1 fetch list carrying
  DISTINCT term-relevance scores — the term-ranked instrument's
  signature; the t1e-era all-0.9 ties are the old exact-value
  instrument's and are cited as old-instrument, never mixed. Protocol as
  declared: budget 12/12, max-rounds 3, model pin daemon :9741
  (Qwen3.6-35B-A3B-MTP-UD-Q6_K draft, Qwen3-Embedding-0.6B-Q8_0 embed,
  tau 0.9), `--backend mock --mock-deck <frozen deck>` — banks read,
  never edited. The one-shot comparator leg's original process was found
  dead mid-run and the leg was re-run per the handoff protocol's
  dead-process branch: 26/26 fresh one-shot artifacts, exit 0 — same
  scorer, same protocol, substitution journaled, never silent. Scored by
  `arms/score-arms.py` (unchanged, C-class) →
  `arms/score-report-t1f.json` (raw preserved; the scoring invocation
  wrote the file one directory up — moved to `arms/` to match the epoch
  convention and the verify gate's expected path). P5 drill: 6/6, no
  noise band (`demo/p5/verify.sh`). DEMO-5 gate:
  `demo/demo5/verify-demo5.sh` — all strips pass (amendments journaled
  below).

- **Verdicts per the declared decision rule** (four verdicts, never
  silent):

  | leg | bar | measured | verdict |
  |---|---|---|---|
  | P4-v0 | >= 58/72 | 53/72 loop, 51/72 one-shot | failed (old-instrument 52/72 at t1e; +1, no bar movement — the retrieval change did not move the v0 coverage bar) |
  | P4-v1 (loop) | >= 12/16 | 9/16 loop, 9/16 one-shot | failed — BUT the instrument hypothesis CONFIRMED: K2 (Gini 0.5469) and K5 (Case-Shiller 325.78), the t1e-journaled "unreachable specific values", are COVERED in the loop arm and ABSENT from the one-shot arm (old-instrument 3/16 loop, 7/16 one-shot at t1e -> 9/16 both arms at t1f) |
  | P3 | >= 10/13 | 12/13 passed (+0 could-not-judge) | passed (t1e 13/13; the v1 pair's p3 failed — the coverage 10 -> 9 drop journaled below) |
  | R-12 | >= 10/12 | 0/12 v0 seeds | failed (structural, unchanged from t1d/t1e: single-origin decks + unweakened floor -> gap sets only grow; v1 trace 1 -> 28 -> 53, not gated) |
  | T1.7 plan presence | all scoped flights carry | 12/12 scoped flights | passed (plan artifacts unchanged by the retrieval change; v1 plan_specifiers ["1980","2024"], 6/6 sub-questions carrying) |
  | P5 | 6/6, no noise band | 6/6 | passed |
  | two-arm lift (pooled) | loop >= one-shot + 0.10 | 1.0 vs 0.979 | failed BY LETTER with the direction flipped — the loop's density (1.0, 35/35) now EXCEEDS the one-shot's (pooled lift 0.02100000000000002); the bar's premise (one-shot at the ceiling) inverted by the instrument change |
  | two-arm lift (v1) | loop >= one-shot + 0.15 | 1.0 vs 0.9473684210526315 | failed BY LETTER with the direction flipped (the one-shot's single untraced claim journaled below) |
  | honesty not worse | loop ungrounded <= one-shot | loop 0.0 vs one-shot 0.02100000000000002 | PASSED — the letter leg passes for the FIRST time (t1e: loop 0.117 vs one-shot 0.0, failed by letter); load-bearing property held: zero untraced figures in [passed] position in ANY arm, both epochs |

  Per-question coverage (loop_covered / oneshot_covered): seed-01 4/6-3/6,
  seed-02 5/6-5/6, seed-03 4/7-5/7, seed-04 4/6-4/6, seed-05 5/6-5/6,
  seed-06 6/6-6/6, seed-07 5/6-5/6, seed-08 5/5-2/5, seed-09 2/6-3/6,
  seed-10 4/6-4/6, seed-11 4/6-4/6, seed-12 5/6-5/6, v1 9/16-9/16.

- **v1 per-key journal (the instrument flip, scorer's reasons verbatim
  from score-report-t1f.json)**: the loop arm covers K2 (0.5469), K4,
  K5 (325.78), K6, K8, K10, K14, K15, K16 — the specific values the
  t1e journal named as unreachable are now REACHED, and the one-shot
  arm (K4, K6, K7, K8, K10, K11, K12, K14, K16) covers NEITHER
  (K2: "missing figures in answer: [('0.5469', None)]"; K5: "missing
  figures in answer: [('325.78', None), ('225', '%')]") — same model,
  same scorer: the loop's term-ranked acquisition surfaced the
  value-bearing document, the one-shot's full-window draft dropped the
  values. The loop arm's 7 uncovered keys are draft
  figure-completeness, not retrieval (each figure sits in the evidence
  window): K1 "missing figures in answer: [('51.9', '%')]" (58.1/50.6/
  50 present), K3 "missing figures in answer: [('7.87', ':1'),
  ('7.81', ':1'), ('172476', None), ('22095', None)]", K7 "missing
  figures in answer: [('4.6', None)]", K9 "no deck-supported form
  (frozen arbiter journal: cannot clear)", K11 "missing figures in
  answer: [('53', '%')]", K12 "missing figures in answer: [('80',
  '%')]", K13 "figures not supported by evidence: [('0.7', 'pp')]".

- **P3 v1 coverage-drop journal (both directions, §18.6)**: the
  scorer's p3_reason for the v1 pair: "round-2 fetched 0 < 20% of
  round-1's 2: True; final coverage 9 >= round-1-evidence coverage 10:
  False" — the round-2 fetch-count half passes, the coverage-not-worse
  half fails (10 -> 9). The scorer's verdicts-note sentence "the v1
  flight passed" is stale t1e-era prose — reproduced verbatim in
  bars.md, never edited (the scorer is frozen), the discrepancy
  journaled here and in bars.md.

- **The instrument strips (verify-demo5.sh) — watched fail -> fix,
  journaled, never silent (§18.1)**:

  1. Strip 3 (figures in passed claims attributable to the evidence
     window) FAILED on the real flight at first: the committed check
     `token in bodies` was LIST MEMBERSHIP against the chunk strings —
     chunk equality, so no token ever matched. demo4's identical code
     never fired because its flight's passed claims carry no figures on
     the stamped line — a latent bug the t1f flight exposed (demo4 not
     re-run; out of scope, journaled here). Fixed: the scorer's OWN
     NUMERIC_TOKEN loaded from score-arms.py (loaded, not copied — one
     decider, §10.6), the citation tail cut at "[Source:", presence =
     substring of the joined window text ("1990" traces inside "the
     1990s" — the window carries the deck verbatim), claim bodies
     joined across the renderer's bullet + continuation lines. Green on
     the real flight: 58 verdict-stamped claims, all figures
     attributable or on flagged claims.
  2. Strip 3b (the concept -> value retrieval proof) as committed
     compared RAW digit runs and failed on the real flight: the round-1
     queries legitimately carry era years (1970..2023 — the R1 prompt
     asks the draft to name years) and generic descriptors ("15-year-old
     homes", "per 1,000 renters") that also occur in the value-bearing
     bodies. Amended: VALUE-SHAPED runs (3+ digits, not 4-digit
     19xx/20xx era years, not all-zero runs) — green: round-1 distinct
     relevance scores [13.0, 14.0, 15.0, 16.0] (no flat 0.9 ties), the
     queries introduce no value-shaped digits beyond the question's own,
     and the admitted hit h11 (top score) carries value-shaped figure
     runs in none of the queries — the concept query retrieves the
     value-bearing document without ever naming its figures. Journaled
     blind spot: 2-digit values (7.87, 9.6, 95/20) are not caught by
     the shape test — the no-bank-vocabulary guarantee is structural
     (the fold-in machinery), not this strip's.
  3. Mode: verify-demo5.sh committed 100644 -> 100755 (demo4
     precedent).

- **The one-shot's single untraced claim — mechanism journaled, not
  absence**: the one-shot arm's density is 0.947 (18/19); the untraced
  claim is the Bridgeport "$560,000+" figure. The figure sits in the
  one-shot's evidence window AND in the frozen deck verbatim — the
  scorer's canonical form strips $ and commas ("560000", None), so
  `\b560000\b` cannot match the window's "$560,000". A canonical-
  matching artifact, not an absence; it sits on a verdict-flagged line,
  never in [passed] position (the load-bearing property held).

- **bars.md carries the scorer's numbers verbatim**
  (score-report-t1f.json — per-question fractions, pooled lift
  0.02100000000000002, all eight legs with the scorer's own notes),
  including two stale verdicts-note sentences reproduced verbatim and
  journaled (P3's "the v1 flight passed" — the pair detail shows the
  v1 coverage drop 10 -> 9; the pooled-lift note's "flagged
  open-question claims stay untraced" — t1f loop density is 1.0,
  nothing untraced).

- **DEMO-5 verdict**: evidence, not the strong demo. The declaration's
  disjunction (P4 clears -> strong demo; hypothesis wrong -> re-cut to
  the bank's key design) resolved to the third outcome: P4 below the
  bars (53/72, 9/16) with the instrument hypothesis CONFIRMED (K2/K5
  reachable — the residual cap moved from retrieval to the draft's own
  figure-completeness: 7 keys the draft omitted figures for while they
  sat in its evidence window). The re-cut decision — bank key design vs
  draft-side work — is the operator's call either way.

## T1-rung-2 corpus search — declaration (order deep-research-t1g, declared 2026-08-15 BEFORE any code change or re-measure)

**The instrument gap, journaled from t1f.** Rung 1 (T1.9) proved term-ranked
retrieval over the DECK surface: a concept query retrieves the value-bearing
document without the loop naming the bank's figures (P4-v1 3/16 -> 9/16, K2
Gini 0.5469 and K5 Case-Shiller 325.78 covered). The deck, however, is the
gym's on-disk fixture. Rung 2 of the operator's acquisition ladder (PLAN.md
§4): the loop's acquisition is wired to the ESTATE's corpus-search surface —
the compounding corpus the T1a demo proved (`svrn corpus ingest` →
`svrn corpus search`, apollo11-evidence). The instrument change: the
acquisition's search leg gains a source dispatch — **mock | corpus**, a
closed set, ONE decider — additive: the mock stays for the bank seeds
(unchanged term-ranked deck surface); the corpus is the second source,
exercised by the v1 report-class flight. The t1f numbers are old-instrument
numbers (deck surface), never mixed with this re-measure.

**§19 survey — what exists and is reused (checked before building anything
new):**

- The corpus RETRIEVAL surface: `CorpusIndex::open` +
  `CorpusIndex::search(&embedding, query, limit)` — vector + FTS hybrid
  (corpus-engine/src/index/search.rs, flat-scan fallback under 10k rows) —
  already wired into the loop's estate survey leg:
  `CliResearchPort::estate_search` (sovereign-cli/src/deep_research_cmd.rs)
  and `svrn corpus search` (corpus_cmd/search.rs). The rung-2 acquisition
  leg calls the SAME port method — no new search engine.
- The corpus STORE surface (how the corpus gets built): `tool:corpus_store`
  (sovereign-tools/src/corpus_store.rs — `CorpusIndex::create` +
  `insert_batch` + `build_indexes` + `mark_indexes_built` +
  `mark_ingestion_complete`), reached at battery time via the shipped
  `notebook` workflow — `svrn corpus ingest <folder> --corpus <id>`
  (corpus_cmd/ingest.rs) — the t1a demo's compounding surface.
- The embed: the daemon's embed slot (model pin
  Qwen3-Embedding-0.6B-Q8_0) — the provider the CLI port already embeds
  with. The mock backend gains an embed fn: CLI runs wire the provider's;
  unit tests wire a deterministic fake (the corpus-engine tests' precedent —
  sharding_round_trip_e2e.rs builds real LanceDB corpora with seeded
  embeddings and FTS-searches them).
- Corpus-hit fetch: the estate IS the evidence store — a corpus hit's
  content is the chunk's own. The port's `web_fetch` resolves the
  `estate:<corpus_id>:<chunk_id>` scheme (the estate_window's existing
  locator convention) from the corpus via `CorpusIndex::get_chunks`; the
  window chunk keeps the hit's custody (personal — the estate's), never
  re-stamped public-web (fetch.rs's unconditional public-web stamp is
  overridden for the estate scheme; SearchHit carries the custody from the
  port's stamp).
- CliResearchPort::estate_search URL mapping fix (same commit, journaled):
  a corpus chunk with no stored url got `estate:<corpus_id>` — identical
  for every chunk of the corpus, so multi-hit estate searches collapsed in
  the window's dedup-by-url. The mapping now carries the chunk id:
  `estate:<corpus_id>:<chunk_id>`. One decider for the mapping.

**Pre-registered source-dispatch semantics (the changed instrument):**

- `SearchSource { Mock, Corpus }` — a closed set (enum, §9), selected ONCE
  per run by the CLI flag `--search-source mock|corpus`, default `mock`
  (additive — the t1f protocol is the default). ONE decider: the flag
  parse; anything else refuses loudly (the backend closed-set rule).
- Mock: `port.web_search(backend, ...)` — the deck term-ranked surface,
  unchanged. Corpus: `port.estate_search(corpus_ids, ...)` — the estate
  corpus-search surface, under the SAME search-budget ledger (family
  `web-search`, key `corpus`, same 12/12 allowance — the protocol is
  unchanged, only the source routes differently).
- Glassbox: the SearchHit `engine` records the source (`mock` | `corpus`);
  the fetch lists, triage outcomes and manifest name the source; the mock
  logs the retrieval decider it ran.

**Red-first shape (watched before the fix, §18.1):** a corpus carrying a
deck's facts, searched by the loop's acquisition with a CONCEPT query (no
figure, no bank vocabulary), must retrieve the value-bearing chunk and its
content must reach the evidence window. Pre-fix (the corpus leg unwired):
the mock's estate_search answers the decked empty — zero hits — the test
fails ("a concept query must retrieve the value-bearing chunk through the
corpus source: []"). Green: the chunk retrieves and its content enters the
window.

**Re-measure protocol (identical to t1f; frozen banks, run never edited):**
`arms/run-arms.sh` — 13 loop flights, budget 12/12, `--max-rounds 3`, model
pin daemon :9741 (Qwen3.6-35B-A3B-MTP-UD-Q6_K draft,
Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9). The 12 v0 seeds: unchanged
(`--backend mock --mock-deck <frozen deck>`, source mock). The v1
report-class flight: `--backend mock --mock-deck bank/v1/deck
--search-source corpus --corpora dr-demo6-v1` — where `dr-demo6-v1` is a
corpus built ONCE, before the flight, from the FROZEN v1 deck bodies via the
estate's shipped ingest surface (`svrn corpus ingest` of a verbatim body
copy under demo/demo6/). Scored by `arms/score-arms.py` (unchanged,
C-class) → `arms/score-report-t1g.json`. The one-shot comparator
(`tests/oneshot_rag.rs` — full-deck window, only the loop differs; the
corpus leg is loop-side) and the P5 6-flight drill unchanged.

**Decision rule (four verdicts per leg, never silent):** P4-v0 ≥ 58/72
(old-instrument 53/72 at t1f); P4-v1 ≥ 12/16 reported (loop arm; one-shot
beside — old-instrument 9/16 both at t1f); P3 ≥ 10/13 not regressed (t1f
12/13); R-12 ≥ 10/12 reported (the structural single-origin shape is
expected unchanged — the floor is never weakened and this change is the
acquisition source, not the floor); T1.7 plan presence all scoped flights;
P5 6/6, no noise band; honesty — zero untraced figures in [passed] position
in ANY arm (the load-bearing property, checked not assumed, never traded
for coverage); two-arm lift reported (pooled ≥ +0.10, v1 ≥ +0.15).

**DEMO-6** (`research/deep-research/demo/demo6/`): the v1 report-class
question rendered by the loop searching the ESTATE — the compounding corpus
carrying the deck's facts — the corpus supplying the evidence: K/N,
attribution, stage strips, verify script. DEMO-6 is the strong demo if
P4-v1 clears 12/16 with zero untraced figures; otherwise it is the
corpus-leg evidence for the landing's fork surface (the bank-key re-cut
remains the operator's call either way).

**The three forks — presented at landing, never decided by this order**
(under the autonomy directive, operator 2026-08-15, goal note d412da5d):
(1) the bank-key re-cut — P4 has failed four consecutive transitions
(52/49/52/53) while the instrument got validated; (2) R-12's structural red
on single-origin decks — 0/12 × 4, the floor never weakened; (3) the
lift-metric ceiling — the two-arm lift failed BY LETTER with the direction
flipped (t1f: loop density 1.0 vs one-shot 0.979/0.947), evidence about the
pre-registered threshold's direction, not about the loop. Each fork's
evidence rides the re-measured numbers; the SEAT decides, logged as
material decisions for the end-of-run audit.

## T1 rung-2 execution journal (order deep-research-t1g)

**Red first — watched fail, then green** (2026-08-15, before any battery
re-measure): the two corpus tests (loop-level
`corpus_source_retrieves_value_bearing_chunk_into_window` in
deep_research/mod.rs; port-level `corpus_surface_retrieves_and_fetches_the_value_bearing_chunk`
+ `estate_urls_without_a_surface_refuse_loudly` in gym.rs) were run with
the corpus surface NEUTRALIZED to the pre-wiring shape (estate_search
answering the decked empty, the dispatch routed but unserved). Red: 72
passed, 2 failed — the corpus source retrieved nothing; the fixture
corpus (built with the corpus-engine tests' precedent — deterministic
seeded embeddings, FTS index) built and indexed fine, and the loop
TERMINATED before round-1 search (`done-partial`, search_calls 0). Then
the surface read was restored: green, 74 passed.

**The red run surfaced a genuine wiring bug — fixed, not papered over.**
`continue_to_web` (the budget gate between audit and acquisition) checked
`decider.remaining(FAMILY_WEB_SEARCH, web_backend)` — the MOCK's key —
while the corpus-source allowance was seeded under `web-search:corpus`
and the per-query spend also used the source key. A corpus-source run
therefore saw zero remaining budget and ended `done-partial` before it
ever searched. Fix: ONE budget-key decider
(`source_budget_key(SearchSource, web_backend)` — `corpus` for the
corpus source, the backend id for the mock), shared by the allowance
map, the continue gate, and the per-query spend. A second derivation
anywhere would recreate the gate/spend disagreement the red run caught.

**Also journaled:** a pre-existing sovereign-core test
(`runtime::grounding::tests::tombstoned_repair_releases_the_audited_draft_with_its_claims_marked`)
failed ONCE in a full parallel suite run and passed in isolation and on
re-run — a suspected parallel tempdir/pid-key collision in that test
(untouched by this order; observed, not fixed — escalate if it recurs).

**The changed instrument is now:** the acquisition source dispatch
(closed set mock | corpus, one decider = the CLI flag parse, additive —
`--search-source` defaults to `mock`), the mock's corpus surface
(`MockBackendImpl::with_corpus` — real `CorpusIndex::search` over
opened estate indexes with the daemon's embed slot, `web_fetch`
resolving `estate:<corpus_id>:<chunk_id>` from the chunk store), the
chunk-level estate locator in BOTH ports (the dedup fix — a
corpus-level-only locator collapsed multi-hit estate searches in the
window's dedup-by-url), `SearchHit.engine` recording the source
(`mock` | `corpus`) and `SearchHit.custody` carrying the port's stamp
through to the window chunk (an estate chunk stays `personal`, never
re-stamped public-web — fetch.rs no longer hardcodes the stamp), and
the budget key shared by allowance/gate/spend. The scorer is untouched;
the battery protocol is unchanged (budget 12/12, max-rounds 3, model
pin); only the v1 flight routes to the corpus source.

**The re-measurement — execution** (2026-08-15; score-report-t1g.json,
the scorer unchanged, C-class). The full battery re-ran identically to
t1f — 13 loop flights (budget 12/12, max-rounds 3, model pin daemon
:9741) + the one-shot comparator (`tests/oneshot_rag.rs`, exit 0,
103.67s, 1 passed) + the P5 6-flight drill. The v1 flight's corpus leg
ran into `v1/` (the t1f-era mock v1 flight preserved untouched as the
control under `v1-mock/`). Verdicts per the declared decision rule
(four verdicts, never silent):

- **P4-v0 51/72 (bar ≥ 58/72) — FAILED**, the fifth consecutive
  measured failure (52/49/52/53/51). The residual now includes the
  corpus triage boundary (below) — the bank-key re-cut fork's evidence
  at landing.
- **P4-v1 2/16 loop vs 11/16 one-shot (bar ≥ 12/16) — FAILED.** The
  corpus flight's mechanism, read from its own artifacts (fetch-list-1,
  triage, evidence-window-1, manifest — all in
  arms/runs/loop/v1/dr-1786853676/): LanceDB's hybrid relevance scores
  QUANTIZE to identical f32 buckets (~0.03333333507180214) for the top
  hit of every round-1 query; the triage's score-then-figure-bearing
  tie-break reads only the TITLE (chunk titles are digit-free document
  names — dead on the corpus surface); the top-k admission degenerates
  to insertion order, and the value-bearing chunks lost a tie lottery
  to thematically-relevant figure-free chunks. The budget (12/12)
  exhausted in round 1, so no round-2 recovery. Result: a 3-chunk
  window carrying none of the bank's figures; the report could not name
  them; 14/16 keys failed "missing figures in answer". The corpus leg
  retrieves (a concept query DOES retrieve the source-report chunk
  carrying "Gini coefficients exceeding 0.54" — direct search probes);
  the R5 triage boundary cannot see past the quantized scores. This is
  the measured boundary the landing's forks must weigh.
- **P3 13/13 (bar ≥ 10/13) — PASSED.**
- **R-12 0/12 v0 seeds (bar ≥ 10/12) — FAILED**, structural, unchanged
  shape, fifth consecutive (the floor is never weakened).
- **T1.7 plan presence 12/12 — PASSED.**
- **two-arm lift pooled 0.938 vs 0.981 (bar +0.10), v1 0.7 vs 1.0 (bar
  +0.15) — FAILED BY LETTER, the direction flipped AGAIN**, this time
  the one-shot side: the corpus flight's thin window left the loop's
  era-year figures untraced. Two consecutive direction flips is the
  lift-metric-ceiling fork's evidence: the threshold's premise
  (one-shot at the ceiling) has not held since the t1d epoch.
- **honesty — FAILED on BOTH the letter AND the load-bearing
  passed-position property, the FIRST epoch where the load-bearing
  property broke.** Letter: loop ungrounded 0.062 vs one-shot 0.019.
  Passed-position: the flight's single [passed] claim restates the
  question's own era ("1980", "2024"), which the window does not carry
  verbatim, and the scorer's own density row flags it untraced
  (traces=false, nums_in_window=[] — the window carries no 1-digit at
  all, so even the citation-machinery tokens do not trace). The era
  years are a traced-once restatement of the question's framing, not
  fabrication — but the decider has no year exemption, and the letter
  is the letter. The t1f journal's "load-bearing property held" must be
  read as epoch-scoped: it held through t1e/t1f, it does NOT hold for
  this corpus flight.
- **P5 6/6, no noise band — PASSED** (demo/p5/verify.sh green).

**DEMO-6 — the declaration's disjunction resolved to the second
outcome.** P4-v1 (2/16) is below the ≥ 12/16 bar, so DEMO-6 is the
**corpus-leg evidence** for the landing's fork surface, not the strong
demo. The demo directory (demo/demo6/) carries the verbatim frozen deck
bodies the corpus was built from (deck-extract/, byte-identical), the
v1 corpus flight's report verbatim, the scorer's bars verbatim
(bars.md), and the verify strips.

**Verify amendments (watched-fail → fix, demo5 precedent, journaled in
the script header):** strip 3's citation tail cut was "[Source:" only —
the corpus renderer cites with estate markdown links and backticked
source refs ("`estate-1` [estate:dr-demo6-v1:<chunk>](...)"), so the
chunk id inside the tail leaked into the claim body and tokenized as a
figure ("64" ×3, measured). The cut now takes the EARLIEST citation
marker of either renderer ("`estate-", "[estate:", "[Source:"). After
the cut, strip 3 FAILS on the era years — the passed-position violation
named, never exempted (the decider has no year exemption). Strip 3b's
a/b halves pass (every round-1 hit engine=corpus; chunk-level estate
locators; window custody personal; no value-shaped digits in the
queries) and its c half FAILS — no admitted chunk carries value-shaped
figures in no query (the triage boundary above; the failure is the
measurement). The script now accumulates verdicts so both designed
failures report in one run, exiting non-zero with the failures named.

## T1 rung-2 instrument changes (order deep-research-t1h) — declared BEFORE the re-measure (§18.6)

Order deep-research-t1h (Phase 1 diagnosis lands first:
research/deep-research/diagnosis/t1h-failure-taxonomy.md; Phase 3 —
these declarations precede ANY re-measure run). The taxonomy classifies
the 37-key union (21 v0 + 14 v1 t1g-canonical + 2 t1f-only): 20+2
Class-A Synthesis figure-omissions, 1 Class-B Synthesis causal-omission
(journaled, not predicted), 11 Class-C Triage corpus-boundary losses, 3
Class-D frozen-arbiter ceiling keys. The three changes below are the
fixes the taxonomy names; each declares its stage, its mechanism, its
predicted recovery, and its red test (watched fail, both at HEAD:
`triage_admits_body_figure_over_figure_free_at_equal_score` and
`draft_prompt_carries_the_window_figure_inventory` — pass 27 / fail 2,
2026-08-16, before any of the changes below were implemented).

### H1 — the corpus-leg triage boundary (repairs stage 6 Triage; predicts 11 Class-C keys)

- The hit surface carries the BODY: `PortHit`, `SurveyHit`, `SearchHit`
  gain `content: Option<String>` (serde default — additive, never a
  schema break). The corpus surfaces fill it (gym.rs estate_search,
  CLI deep_research_cmd.rs estate_search — one shape); web hits keep
  None; the loop's conversions (PortHit→SurveyHit, PortHit→SearchHit)
  carry it.
- `figure_bearing` (acquisition.rs) extends to title+snippet+content —
  ONE decider preserved (§10.6); the corpus surface's digit-free
  titles and term-centered snippet cuts no longer blind it.
- `estate_window` (mod.rs) drafts from content-or-snippet
  (`hit.content.clone().unwrap_or_else(|| hit.snippet.clone())`) so
  admitted bodies reach the draft.
- Predicted recovery: the 11 Class-C keys (K1, K2, K4, K5, K6, K7,
  K10, K11, K12, K15, K16) — deterministic admission of chunk-65-type
  hits inside the quantized 1/30 top bucket; the battery measures the
  count. The 13/16 content ceiling stands (K3/K9/K13 remain
  unreachable under the frozen scorer — Class D, the bank-key fork).

### H2 — draft figure-completeness (repairs stage 9 Synthesis; predicts 20+2 Class-A keys)

- `draft_round` (synthesize.rs) appends a deterministic figure
  inventory to BOTH round shapes: per window chunk, its
  `figure_tokens` (mod.rs — the ONE figure decider), under the header
  "Figures present in the evidence:", with the instruction that every
  evidence-supported figure must appear in the answer. Empty window →
  no inventory block (nothing to enumerate, nothing to invent).
- The inventory is code-enforced into the PROMPT; the model's carrying
  of the figures into the answer is measured by the battery, never
  assumed (§7.6).
- Predicted recovery: the 20 Class-A keys + the 2 t1f-union keys
  (seed-09 K3, seed-10 K4). Class B (seed-01 K4, causal elements) is
  journaled, not predicted — no deterministic entity carrier.

### Honesty — the witness numeric-specificity rule (repairs stage 10 Claim gate; the constitution, never traded)

- `witness_presence` (containment.rs) — when the claim's specifics
  include numeric-class specifics (figure_tokens non-empty), at least
  one numeric specific must be present for the witness to fire;
  thematic presence alone can no longer mask numeric absence.
- Downgrade-only: the rule never converts a pass into a fail; a
  c1-type claim becomes CouldNotJudge at most (the all-absent
  downgrade path, audit.rs). The floor/witness are never weakened.
- Predicted: zero untraced figures in [passed] position in ANY arm —
  the t1g break (c1 restating "1980"/"2024" the window does not carry,
  traces=false, nums_in_window=[]) must not recur.

### Honesty amendment — claim-figure tokens, the partial-trace shape (order deep-research-t1h, declared 2026-08-16 BEFORE any re-measure — the probe was instrument validation, not measurement)

The probe (/tmp/dr-probe/dr-1786928663, throwaway — NOT part of the
battery) validated the landed H1 + numeric-class rule end-to-end and
found the PARTIAL-TRACE shape the numeric-class rule cannot see: the
final c1 passed with witness specifics ["1980","2000","University of
Georgia"] while the claim itself carried "2024" in "(1980–2024)" and
the round-2 evidence (fetch ev-1..3 + estate chunks 21/29/33/4/50/64)
carried "1980" (chunk 50: "…transformations since 1980,") and "2000"
but NOT "2024" (verdict-set.json c1, gap-list-2.json,
evidence-window-1.json). The extractor DROPPED "2024"; the
numeric-class rule checks the EXTRACTED specifics only, fired on
"1980", and the claim passed with an untraced figure. The scorer's
sentence-level ANY trace (score-arms.py:531) would show honesty green;
the constitution's letter — zero untraced figures in [passed] position
— fails. The instrument is strengthened to the claim's OWN figure
tokens.

- `missing_claim_figures(claim, evidence) -> Vec<String>`
  (containment.rs) — C-class, extraction-independent: every claim
  figure token (figure_tokens — the ONE figure decider) absent from
  the evidence (citation spans stripped, heading-shaped lines do not
  count, deduplicated). The claim's own text cannot drop its figures.
- Short-circuit at the TOP of `containment_witness`, after the
  empty-claim/chunks guard, BEFORE extraction: any claim figure absent
  from the evidence → `WitnessOutcome{ran: true, all_absent: true,
  reason: "claim figures absent from the evidence — untraced: <list>"}`.
  Covers BOTH polarities — a negative claim with absent figures is
  unverifiable (downgraded, never passed).
- Downgrade-only: this path only ever REMOVES the witness's fire (→
  the all-absent CouldNotJudge); the floor/witness are never weakened;
  the numeric-class rule over the extracted specifics is unchanged.
- Red tests (watched fail before implementation — the tests referenced
  the nonexistent symbol at HEAD): `untraced_claim_figure_is_downgraded_
  not_passed` (audit.rs, the probe c1 shape — era_window() fixture
  carries "since 1980,"/"after 2000" but NOT "2024", two distinct
  origins so the floor passes; scripted extract "1980\nUniversity of
  Georgia"; asserts CouldNotJudge + reason names "2024");
  `claim_figures_are_extraction_independent` + `untraced_claim_figure_
  is_reported_absent` (containment.rs units — en-dash splits
  "1980–2024", span-only figures count absent); positive control
  `fully_traced_claim_figures_do_not_block_the_witness` (extract
  "2000\nGoverning" → Passed — the strengthen never removes a true
  positive); negative shape `negative_claim_with_untraced_figures_is_
  downgraded_not_passed`.
- Predicted: zero untraced figures in [passed] position in ANY arm,
  INCLUDING the partial-trace class (a claim figure the extractor
  dropped); the constitution holds in both the t1g red shape (zero
  digits in the window — landed H1 + numeric-class rule) and the probe
  partial shape (claim figure absent while other claim figures trace).

### Honesty amendment 2 — the claim-side citation strip (the citation-leak class; declared 2026-08-16 BEFORE the re-measure that follows)

The battery's first loop flights (seed-01..seed-05, run started
2026-08-16) caught a defect class the probe could not: every claim
citing `ev-1`-style evidence ids was downgraded "untraced: 1" — the
digit came from the CLAIM'S OWN citation tail ("[Source: ev-1]"), not
the claim's content. The evidence side was stripped before the figure
check; the claim side was not. (The probe's claims carried digit-free
spans — "University of Georgia" — so validation passed.) The flights
were stopped mid-run; the class is fixed RED-FIRST:
`claim_citation_spans_do_not_leak_figures` (containment.rs — watched
fail `left: ["1"]` vs `right: []`, then green): `missing_claim_figures`
now strips citation spans from the claim before tokenizing its figures
— ONE strip contract, both sides. The stopped flights are invalidated
(never scored); the battery below re-ran from a clean state with the
fixed binary. No measurement exists under the leak.

### Execution record — Phase 3 (append-only, written after the re-measure)

Battery protocol unchanged: frozen banks (v0 mint b28c72b7 + v1 mint
e63a1449…), 13 loop flights (budget 12/12, max-rounds 3, model pin
daemon :9741 Qwen3.6-35B-A3B-MTP-UD-Q6_K draft / Qwen3-Embedding-0.6B-Q8_0
embed, tau 0.9), one-shot comparator, P5 6-flight drill; score-arms.py →
score-report-t1h.json; bars reported four-verdict; DEMO-7 recorded in
research/deep-research/demo/demo7/; dr-local-loop transition written
per the measured outcome at landing.

## Execution record — Phase 3 measured (append-only, written after the re-measure)

Run 2026-08-16. All instrument changes landed and pre-registered ABOVE
(this section's header) BEFORE the re-measure: H1 (hit surface carries
the body; figure-bearing decider reads title+snippet+content), H2 (the
deterministic figure inventory in both draft shapes), the witness
numeric-specificity rule, the claim-figure short-circuit (amendment 1),
and the claim-side citation strip (amendment 2 — caught RED-FIRST on
the battery's first flights; seed-01..05 stopped, invalidated, never
scored; the battery re-ran clean from the fixed binary). The battery:
13 loop flights (12 v0 mock + v1 corpus into
arms/runs/loop/v1/dr-1786933992, newest epoch wins the scorer slot),
all terminal done-partial; one-shot comparator exit 0; P5 6-flight
drill + demo/p5/verify.sh green.

### Measured legs (score-report-t1h.json, C-class scorer; four verdicts)

| leg | t1g | t1h | bar | verdict |
|---|---|---|---|---|
| P4-v0 | 51/72 | 63/72 | >=58/72 | **passed** — first pass in six measurements (52/49/52/53/51/63) |
| P4-v1 (loop) | 2/16 | 3/16 | >=12/16 | failed — K16 recovered (35%, 31%, 19%); 10 Class-C + 3 Class-D stand |
| P3 | 13/13 | 12/13 | >=10/13 | passed — v1 corpus flight failed the ratio clause (round-2 fetched 3 = round-1's 3, not < 20%): the corpus source makes the loop churn rather than converge; journaled |
| R-12 | 0/12 | 0/12 | >=10/12 | failed — structural, sixth consecutive |
| T1.7 plan presence | 12/12 | 12/12 | all scoped flights carry | passed |
| two-arm lift (pooled) | 0.938 vs 0.981 (-0.043) | 0.977 vs 0.977 (0.0) | +0.10 | failed by letter — direction no longer flips; the spread is exactly zero |
| two-arm lift (v1) | 0.7 vs 1.0 | 1.0 vs 1.0 | +0.15 | failed by letter |
| honesty letter | 0.062 vs 0.019 (failed) | 0.023 vs 0.023 | loop <= one-shot | **passed** |
| honesty load-bearing | FAILED (first epoch the property broke) | zero untraced figures in [passed] position | ANY arm | **passed — artifact-verified** (below) |
| P5 | 6/6 | 6/6 | no noise band | passed — verify.sh green; the fabrication-absent strip asserts 0 passed claims (under the strengthened witness every drill claim is downgraded — the asserted count, journaled) |

### Prediction vs outcome (the diagnosis's H1/H2/amendments)

- H2 (draft figure-completeness; predicted 20 Class-A + 2 t1f-union):
  **15 of 22 recovered** — seed-02 K4, seed-03 K4/K6/K7, seed-04 K4/K5,
  seed-05 K4, seed-07 K3, seed-08 K2/K3/K5, seed-09 K2, seed-10 K5,
  seed-11 K4, seed-12 K6. Still missed: seed-01 K2/K3, seed-09 K4/K6,
  seed-11 K2, and both t1f-union keys (seed-09 K3, seed-10 K4). Class-B
  (seed-01 K4) still missed — journaled, not predicted.
- H1 (corpus-leg triage boundary; predicted 11 Class-C): **1 of 11
  recovered** (K16). The quantized 1/30 f32 bucket (every round-1 score
  0.03333333507180214) still degenerates admission to insertion order —
  the t1g mechanism persists even with the body-figure decider (the
  ties break within a bucket whose members all now carry figures).
  Class-D (K3/K9/K13) unreachable under the frozen scorer, as predicted
  (13/16 ceiling stands).
- Drift (covered at t1g, missed at t1h — loop variance, no attributed
  mechanism): seed-06 K1, seed-06 K3, seed-11 K6.

### The honesty load-bearing property — artifact-verified, not scorer-note-verified

The scorer's honesty note ("zero untraced numbers sit in [passed]
position in ANY arm") is a FIXED string in score-arms.py (it still
cites "t1e loop 0.117 < t1d 0.497" — stale by construction; at t1g it
printed while the property had broken, which the t1g transition itself
corrected in prose). The t1h verdict therefore rests on an independent
artifact-level check, not the note: every PASSED-position claim's
figure tokens (maximal digit runs, citation spans stripped — the
journaled decider) across every flight artifact in the arms tree —
96 runs (all epochs t1d..t1h, loop + P5) plus the one-shot reports
against their window JSONs — verify against the run's accumulated
evidence. Result: zero untraced figures in passed position, with one
excluded artifact: arms/runs/loop/v1/dr-1786853676, the t1g-era v1
flight whose passed-position violation IS the journaled t1g failure
(first epoch the property broke) — history, not a t1h measurement. The
one-shot arm carries no verdict stamps (vacuous by format); its
honesty is the scorer's letter leg (0.023 vs 0.023, passed).

### The measured query-side finding (strip 3c)

The v1 corpus flight's round-1 gap-template query q1 (formed from the
survey answer's gap row g2) carries the value-shaped run "100" — the
survey answer (model) quoted the estate's own admitted chunk
(terry-uga, "the nation's largest 100 cities") and the gap-template
carried the figure verbatim into the query. The figure traces to the
admitted window (attribution intact); the query-side anti-leak shape
(DEMO-5/6 strip 3c) is violated by measurement on this flight — the
DEMO-7 verify fails that strip, naming "100" (verdicts accumulate,
Amendment C). The mechanism: the survey gap-formation now quotes the
estate's bodies — the same H1 surface that fixed the triage feeds the
query formation. Journaled, never silenced.

### Red-first compliance

All five instrument tests watched red before green (the compile-red
missing-symbol shape; the citation-leak `left: ["1"]` vs `right: []`
behavior-red on the amendment-2 fix). The battery's own first flights
caught the leak class the probe could not (digit-bearing citation
tails); the class is pre-registered (amendment 2) and the invalidated
flights were removed, never scored. No measurement exists under the
leak.

---

## T2a egress boundary — declaration (order deep-research-t2a, declared 2026-08-16 BEFORE any code change or flight, §18.6)

The order's lane is Boundary-first: the two red-first tests (R-5, F26)
already land the named reds before the boundary code, and the census is
validated before it measures. This section pre-registers the instrument
changes that follow — the rung-3 search source, the run-scoped consent
gate, and the SpendDecider frontier-key arm — before any flight (the
DEMO-8 flight at the end of the order).

### Instrument 1 — the rung-3 search source (closed set grows: `mock | corpus | web`)

`SearchSource` (sovereign-core `deep_research::mod.rs`) gains a `Web`
variant: `parse "web"` / `as_str "web"` / source budget key `"web"`.
`acquire_round`'s dispatch treats `Web` identically to `Mock` — the
port's `web_search` leg. The port already stamps web hits
`Custody::PublicWeb` (code, never a model — R-2/R-6); the rung-3
instrument change is the closed-set variant + dispatch arm, nothing
else. A `--search-source web` run egresses queries to the configured
web backend; a `--backend mock` + `--search-source web` run serves the
web leg from the deck (no egress — the P5 drill shape).

### Instrument 2 — the run-scoped consent grant (default-deny)

The egress boundary (`sovereign-core/src/egress.rs`, the ONE choke
point for remote-model calls AND query egress) releases a payload iff:

1. the payload's custody is `PublicWeb` (the bar's unconditional
   release), OR
2. a run-scoped `ConsentGrant { run_id, granted_at_unix,
   release_floor: Custody }` covers the payload's custody (floor
   `personal` covers all; `peer` covers peer + public-web; `public-web`
   covers public-web only), OR
3. the payload is a QUERY formed verbatim by the user (the tool path —
   `user_formed`, declared by the caller, never by a model) — the
   user's own words leaving at the user's own action.

Everything else refuses, typed, naming what was withheld (custody
class, what was leaving, the missing/insufficient grant). `Unknown`
custody always refuses. Every egress event — released or refused — is
traced at `tracing=debug` (provider/target, payload class, custody
proof, grant run id when one released it).

The CLI verb gains `--consent <class>` (`public-web | peer | personal`
— the closed set, a release floor). The grant is frozen into the
charter at launch (FR-3), carried by the port AND recorded in the run
manifest (`manifest.json` gains `consent`). Default: no flag, no grant,
web-search egress refuses — the DEMO-8 refusal case. Enrich's
`--provider` dispatch absorbs the same boundary with the client's
defaults `payload_custody = Personal`, `consent = None` — the R-5 red
turns green with zero assertion changes.

### Instrument 3 — the SpendDecider frontier-key arm (inert until t2b)

`SpendDecider` (sovereign-core `deep_research::budget.rs`, the ONE
run-scoped fail-closed budget decider — R-6) gains `FAMILY_FRONTIER_KEY`
("frontier-key") with the fail_closed_table test extended to pin it.
The arm is INERT until t2b wires the frontier-judge dispatch; nothing
in t2a calls it. The studio decider is REMOVED in the same commit:
`budget_allows` (orchestrator.rs), `SelectInputs.budget` +
`BudgetView` (web/search), `check_budget` / `decrement_budget`
(search.rs). The R-6 census's forbidden identifiers
(`budget_allows`, `decrement_budget`, `BudgetView`) go to zero across
every production src tree, and the census gains a path-scoped check
that `sovereign-tools-base/src/search.rs` no longer contains a budget
decider (the global identifier check cannot forbid `check_budget`
because sovereign-core's conversation-FRAME check is a different
concept).

### Instrument 4 — the census's boundary row (the same commit as the boundary)

The F26 registry changes WITH the boundary code, in the same commit
(review-moment contract): `egress.rs` is registered `Boundary` with its
construction count; `inference_client.rs` drops `RemotePayload` 4 →
`LocalDaemon` 3 (the chat client construction moves into the
boundary); `deep_research_cmd.rs` drops `QueryEgress` 2 → `LocalDaemon`
1 (the probe); `knowledge_lookup/mod.rs` loses its construction (row
removed); the studio `web/mod.rs` row reclassifies to the fetch site
(`InboundOnly`). The census is the instrument — it fails on any
registry/count drift, and the gate is the two reds re-run green.

### What is NOT changing

The bar texts (dr-egress, dr-budget-one-decider) are frozen; only the
transitions flip, on measured evidence. The deck/mock flight path
egresses nothing (its search/fetch legs are local) — no consent
required there, and the measurement batteries are unaffected. The
`check_budget` name stays legal globally because the conversation-FRAME
schema check (frame.rs/conv_frame.rs/session_state.rs) is a different
concept; the tools-base search decider is removed by path.

### Amendment 2026-08-16 (same day, BEFORE the gates — the landing review's discoveries)

Three findings from the landing review, recorded before any gate or
flight (append-only, §18.6):

1. **The desktop Search-the-web card carried an egress-class client at
   the red.** The census's red registry counted
   `sovereign-desktop/.../commands/conversation.rs` as `LocalDaemon 1`
   — the landing re-home review found that site is the
   Search-the-web card's orchestrator client, dispatching External
   queries (DDG/Brave/Tavily) — an egress-class construction the red
   census mislabeled. Correction: the red was FIVE egress-class sites,
   not four (inference_client, deep_research_cmd, knowledge_lookup,
   web/mod, conversation.rs). The landing re-homes conversation.rs's
   construction into the boundary (`egress::search_client()`, injected
   like every host), routes its query egress through `verify` with
   `user_formed: true` (the card click IS the user's own action with
   the user's own words — the release rule's clause 3 covers it
   without a grant), drops its `BudgetView` usage with the decider
   removal (Instrument 3's "zero across every production src tree"
   covers it), and the row is removed with the site.

2. **`model_client` takes the timeout as a parameter.** Enrich's chat
   path carries its documented 1800s hang headroom (Phase-1 extract on
   a 27B model legitimately runs 5–15 minutes; the 120s draft default
   would have silently killed real campaigns — verified 2026-04-25).
   The boundary's remote-model factory is
   `model_client(timeout: Duration)` — one construction site, caller
   names the policy. The census `Boundary` row still counts 2 sites.

3. **The decider removal's full wake.** Beyond orchestrator.rs and
   search.rs: the `BudgetView` re-export in `web/search/mod.rs` was
   dropped; the two real-network e2e tests (duckduckgo_real_e2e,
   tavily_real_e2e) lost their budget-view usage (TestOnly — compile
   surface only); `WebSearchTool::new` was removed (its only caller,
   the recipe-agent live trial, now injects the boundary client); the
   three production hosts (server main.rs, desktop state.rs, chat
   bootstrap) inject `egress::search_client()` at construction. No
   count in Instrument 4 changes beyond the conversation.rs row above.

4. **The release rule's `covers` comparison was inverted — the gate
   caught it before any flight.** `ConsentGrant::covers` compared
   `restrictiveness(payload) >= restrictiveness(floor)`; with
   PublicWeb=0 < Peer=1 < Personal=2 that released a *personal*
   payload under a *public-web* grant — the exact inversion of the
   pre-registered contract (Instrument 2: "`public-web` covers
   public-web only"). The unit test
   `grant_floor_covers_and_refuses_by_class` (already in the tree,
   unchanged) failed: `verify` returned `Ok(())` where the pinned
   semantics require a typed refusal. Fix: `<=` in `covers()`
   (egress.rs), implementation aligned to the declared contract;
   no test weakened, no assertion changed. Recorded here BEFORE the
   DEMO-8 flight (append-only, §18.6).

### Amendment 2026-08-16 (post-gate, pre-flight — the egress-trace observability gap)

The gate chain verified the boundary's code paths; the DEMO-8 flight
would have been the first live egress. One gap found while preparing
the flight's trace capture, recorded BEFORE any flight (append-only,
§18.6):

**The deep-research verb installs no tracing subscriber.** The egress
boundary's contract (egress.rs module doc: "Every egress event —
released or refused — is traced at `tracing=debug` under this
module's target") is unobservable on a flight: `cmd_deep_research` is
reached from a dispatch arm in sovereign-cli `main.rs` that never
calls `init_tracing`, so the boundary's `debug!` events compile to
no-ops. Instrument change (the demo's "egress trace in the
artifacts" needs the trace to exist): the `deep-research` dispatch
arm calls `util::tracing_init::init_tracing("sovereign_cli=info,sovereign_core::egress=debug")`
— the egress decisions (released, with provider + payload class +
custody + grant run id + floor; refused, with what was withheld) are
visible at debug on EVERY flight by default (glassbox, ARCH §9), and
`RUST_LOG` still overrides (the flight captures stderr with the
filter explicit). No decision path changes; observability only.

## Execution record — T2a (append-only, written after the flights)

Order deep-research-t2a, lane Boundary-first. Every instrument change
above was declared and recorded BEFORE any flight; this section is
written after. Nothing below re-negotiates a pre-registered contract.

### What ran, in order

1. **The reds, red-first.** R-5 (`sovereign/crates/sovereign-cli-llm/
   src/enrich_cmd/egress_reds.rs:96` — a personal-corpus chunk must
   not reach a remote payload via `enrich --provider`) failed at HEAD
   and passes with ZERO assertion changes after the fix. R-6 (the F26
   census) counted FIVE egress-class remote client construction sites
   outside any boundary at HEAD (inference_client, deep_research_cmd,
   knowledge_lookup, web/mod, conversation.rs); after the landing the
   census registers ZERO outside the boundary and the r6 gate scans
   every production src tree for the retired fail-open deciders
   (`budget_allows`, `decrement_budget`, `BudgetView`, path-scoped
   `check_budget`) and reads zero — enforced as a build gate in the
   standard test suite.
2. **The ONE boundary** (`sovereign-core/src/egress.rs`): the single
   choke point for remote-model calls AND query egress, with the
   release rule pre-registered in Instrument 2 — public-web custody
   releases unconditionally; a run-scoped ConsentGrant releases what
   its floor covers; a user-formed query releases; everything else
   refuses typed, naming what was withheld; Unknown always refuses.
   The landing review discovered the release-rule inversion (`>=` in
   `covers()`, released personal under a public-web grant): the
   pre-existing unit test caught it, the implementation was aligned to
   the declared contract (Amendment 4), and the flight evidence below
   measures the corrected rule.
3. **The one decider** (`deep_research::budget`): run-scoped,
   fail-closed, hash-seeded at launch (mod.rs), every decision
   journaled to the run's budget-ledger.json (the ICD artifact); the
   frontier-key family declared and INERT until t2b — no allowance is
   ever seeded (the `fail_closed_table` test pins
   `no-allowance-or-exhausted`).
4. **Two flights of the SAME question and source**, differing in one
   flag, after the observability amendment made the boundary's
   decisions visible at `tracing=debug` on every flight:
   - dr-1786940564, `--search-source web`, no `--consent`: the first
     query egress REFUSED before any request was built — `egress
     refused: query with personal custody to tavily — no run consent
     grant — the boundary is default-deny for non-public-web payloads
     (grant absent — default-deny)`. Exit 1, loud; run dir has NO
     fetch list (zero acquisition spend, zero egress); the ledger
     journals the single attempted spend (the allowance unit is
     consumed by the attempt, recorded first).
   - dr-1786940569, `--search-source web --consent personal`: the
     grant minted once at launch (run id + floor, frozen in the
     charter, recorded in the manifest, carried by the port); egress
     trace shows 4 query releases under the run grant and 4 url
     releases on public-web custody; every hit engine `web`, every
     fetched source and every evidence-window chunk custody
     `public-web`; ledger 8 allows to exactly 0 remaining across both
     families; terminal `done-partial` with truncation declared; 21
     verdict-stamped claims (1 passed, 20 could-not-judge).
5. **The transitions.** dr-egress and dr-budget-one-decider written
   `to = "met"` on 2026-08-16 with the measured evidence in
   `quality/initiative-bars.toml` (validated: 13 dr- bars, both met
   transitions present, 62 additions / 0 deletions vs HEAD); the bar
   transitions carried verbatim into DEMO-8's bars.md, never
   hand-typed.

### Measured verdicts

| Gate | Result |
|---|---|
| R-5 red-first test | failed at HEAD → passed, zero assertion changes |
| F26 census (r5 boundary row) | 5 outside-boundary sites at HEAD → 0 after landing, build-gated |
| F26 census (r6 decider gate) | retired identifiers read 0 in all production src trees |
| egress unit suite | 9/9 incl. `grant_floor_covers_and_refuses_by_class` |
| refusal flight | exit 1, typed refusal naming what was withheld, zero egress |
| consent flight | exit 0 done-partial, 8 allows journaled, custody stamps on every chunk |
| verify-demo8.sh | all 7 committed-artifact strips pass; live strips (current binary: refusal re-run exit 1 + typed refusal; raw run dir: hits/chunks all public-web) pass |
| lint --full | exit 0 (13.5s warm) |
| test --full | 9907 pass / 0 fail, exit 0 |

DEMO-8 (research/deep-research/demo/demo8/) holds the flight artifacts
— report, manifest, charter, egress trace, refusal transcript + exit
code, both ledgers, bars.md, and the verify script that re-checks them.

### What the boundary refused to do

The consent flight's web evidence could not attribute a single claim
to the web's figures (corroboration floor + extracted-specifics-absent
— 20 of 21 claims could-not-judge, declared in the report). The loop
did NOT render the web's numbers as facts it could not support. That
is the honesty machinery doing its pre-existing job on a NEW source —
the honesty constitution is untouched by this order (t2b carries the
DRB re-measurement arms).

---

## T2b — the DRB arms: P2 between-arm measurement, P1 named proxy, the restated kill bar (order `deep-research-t2b`) — DECLARATION

Order `deep-research-t2b` executes PLAN.md §4 T2's measurement half and the
DeepResearch Bench external holdout. This section is the declaration; the
execution record is appended below it after the flights (append-only). The
seat verifies this ordering at landing (§18.6). Appended 2026-08-16
(~22:10Z), BEFORE the first DRB flight.

### 1. What is measured

- **P2** — FACT citation-accuracy fabrication rate, between the two arms on
  the frozen DRB subset, with pre-registered n = 10 and a cluster-adjusted
  CI, against the published leaderboard reference.
- **P1** — the local cost arm vs a NAMED proxy (o3-deep-research API at
  $1.45/task, frozen in `drb/p1-cost-reference.md`), never "cloud DR" in
  general.
- **The kill bar** — `dr-verdict` (PLAN §6 bar 8, text FROZEN): **ship iff
  P4 AND P2 AND P1.** If the bar fails, the measurement IS the deliverable.
  Honesty is never traded to move P2 (kill, revert).

### 2. The frozen subset (n = 10, content-blind)

- Population: the 50 English tasks of the DRB prompt set
  (`drb/query.full.jsonl`, 100 rows vendored verbatim).
- Method: seed string `deep-research-t2b-drb-subset-2026-08-17`;
  seed = int(sha256(seed_string)[:8], 16) = 556953489;
  rng = random.Random(seed); subset = sorted(rng.sample(en_ids, 10)).
  Reproducible: `python3 drb/select-subset.py`. The selector reads ids only
  — content-blind.
- **Subset: ids [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]** (all
  language = "en"; topics span Finance, Science & Technology, Software
  Development, Health, Hardware, Crime & Law, Religion). Frozen in
  `drb/query.subset.jsonl`.
- Freeze rule: every file under `research/deep-research/drb/` listed in
  `SHA256SUMS` is FROZEN at this declaration — vendored verbatim, never
  edited after this point. Any later edit is a NAMED amendment (§18.6) with
  a reason in the execution record, never silent. `verify-demo9.sh`
  re-checks the hashes at landing.
- Provenance: benchmark repo Ayanami0730/deep_research_bench commit
  `469cce54ea7f6a63c163d3d9fec879cf289ec484` (cloned for local vendoring —
  read-only inspection, nothing uploaded); leaderboard CSV fetched from the
  official space data dir 2026-08-16; paper arXiv 2506.11763 v2 (ar5iv).

### 3. The judge-choice fork — DEFAULT LOCAL (pinned)

- **The pinned judge**: the daemon's pinned deep-research draft model —
  Qwen3.6-35B-A3B-MTP-UD-Q6_K — on daemon `:9741` (local; verified up and
  answering with the vendored payload before this declaration). One pin,
  chosen once, never swapped mid-measurement; the same daemon and pin serve
  both arms.
- Judge path: the vendored openai-compat client
  (`drb/vendor/utils/api.py`: `LLM_BACKEND=openai`,
  `OPENAI_BASE_URL=http://127.0.0.1:9741/v1`, `FACT_MODEL=<pin>`) driving
  the vendored `validate.py` English prompt (3-verdict
  supported/unsupported/unknown; reference-with-no-valid-content → all
  unknown; 3 retries; unparseable-after-retries → `validate_error`, row
  dropped from stats).
- **Named fallback (§18.3)** — fires ONLY if FACT cannot run locally: the
  judge probe (10.b) fails all retries or the daemon is unreachable at
  judge time. The fallback judge is then named in the execution record
  before it is used. Bank vocabulary never enters prompts, and the judged
  statements never mention the bank.

### 4. The two arms (paired on the same frozen 10 tasks)

Both arms: `--backend auto --max-rounds 3`, model pin daemon `:9741`
(every flight's manifest records the pin and the consent). One arm does not
start before the other finishes (no shared-state coupling; the local arm
first, then the hybrid arm, so the corpus leg cannot benefit from web
evidence and the web leg cannot reuse corpus evidence).

- **local arm** (the corpus + rung-2 leg):
  `sovereign deep-research "<question>" --backend auto --search-source corpus --corpora wikipedia --search 12 --fetch 12 --max-rounds 3 --run-dir drb/runs/local/drb-<id>`
- **hybrid arm** (rung-3 web leg through the t2a boundary):
  `sovereign deep-research "<question>" --backend auto --search-source web --consent personal --search 4 --fetch 4 --max-rounds 3 --run-dir drb/runs/hybrid/drb-<id>`
  — the run-scoped consent grant (release floor: personal, the demo8
  class), minted at launch, frozen into the charter; every web fetch lands
  with public-web custody.

A flight whose manifest does not reach a terminal state is re-run once; a
second failure is recorded with its cause and the task still scores from
whatever `verdict-set.json` exists (an absent verdict set → the task has no
citable pairs).

### 5. The scorer's named substitutions (DRB stages → our run artifacts)

- **extract** (report → (fact, url) pairs) ← the run's own claim store:
  `verdict-set.json` claims. `fact` = the claim text with its `[Source:
  …]` citation tail stripped (deterministic span strip — the same citation
  apparatus the honesty machinery removes on the claim side); `url` = each
  distinct citation url the claim cites (citations[] entries carry
  `{evidence_id, url, chunk_id}`); one (claim, url) pair per citation —
  the official rule "one fact citing 2 refs → 2 triples". The official
  `ref_idx` (reference-list index) is not used: reference resolution is by
  evidence id, not the paper's reference list.
- **deduplicate** — the run's claims are already deduplicated by the loop's
  own claim machinery; the scorer only collapses identical (normalized
  claim text, url) pairs deterministically (no LLM dedup stage — named
  deviation from the official pipeline, recorded here).
- **validate** — per unique url, the vendored English prompt; the
  statements are the stripped claim texts; the reference is the
  evidence-window chunk content for that url (id-matched across rounds;
  content already under the loop's 12,000-char cap), falling back to the
  fetch-list search-hit snippet for the url, else NO reference → all
  statements for that url judged `unknown` (the official
  no-valid-content rule). Three verdicts, 3 retries, `validate_error`
  rows dropped (official rules).
- **stat** — the vendored pooled definition (valid / (valid+invalid) over
  non-unknown pairs) AND the paper's per-task definitions
  (Acc_t = N_s,t / N_u,t; N_u,t = 0 → Acc_t = 0, Eq 4-5).

### 6. P2 — metric definitions, cluster-adjusted CI, decision rule

- **Primary statistic per arm**: pooled fabrication rate =
  `unsupported / (supported + unsupported)` over non-unknown (claim, url)
  pairs — the vendored `stat.py` convention, i.e. the definition the
  published leaderboard values are on (cross-check frozen in
  `drb/paper-fact-definition.md`: 0.8732 ↔ 87.32).
- **Per-task rates** (for the bootstrap and the report): for task t,
  fabrication_t = unsupported_t / (supported_t + unsupported_t);
  N_u,t = 0 → fabrication_t = 1 (mirror of Acc_t = 0).
- **Cluster-adjusted CI**: tasks are the clusters (claims cluster within
  tasks). Nonparametric cluster bootstrap: resample the 10 tasks with
  replacement; for each resample, recompute the pooled fabrication rate
  over the resampled tasks' pairs; 10,000 resamples; rng seeded —
  seed = int(sha256("deep-research-t2b-bootstrap-2026-08-17")[:8], 16);
  95% CI = 2.5th/97.5th percentiles of the resample distribution. A
  resample with zero non-unknown pairs is undefined and dropped (recorded
  in the report if > 1%).
- **References** (frozen `drb/leaderboard.csv`, official): primary =
  perplexity-Research, citation_accuracy 82.63 → fabrication 0.1737;
  secondary = openai-deepresearch, 75.01 → 0.2499. Both reported; the gate
  is on the primary.
- **Decision rule (four verdicts)** on the HYBRID arm vs the primary
  reference (the leaderboard line is fixed — no sampling error of its
  own):
  - met — hybrid CI upper bound < 0.1737;
  - failed — hybrid CI lower bound ≥ 0.1737;
  - could-not-judge — the CI straddles the reference (overlap;
    underpowered);
  - never-ran — either arm did not complete.
- **Between-arm delta** (hybrid − local, same bootstrap CI) is reported
  descriptively; it is the measurement's body, not its gate.

### 7. P1 — metric definition and decision rule

- **Measured cost per task** = wall_seconds × 60 W × $0.15/kWh / 3600
  (named constants, frozen in `drb/p1-cost-reference.md`; wall from the
  manifest lock: released − acquired). Raws reported: wall time,
  acquisition units (searches + fetches), report words.
- **The named proxy reference** = $1.45/task — OpenAI o3-deep-research
  published API rates ($10/M input, $40/M output, $10/1K web searches) at
  the typical mix (50K input / 20K output / 15 searches = 0.50 + 0.80 +
  0.15), cited to the OpenAI Developer Community announcement and the
  TokenCost analysis (URLs frozen in `drb/p1-cost-reference.md`).
- **Decision rule (four verdicts)**: met iff
  max(local_mean_cost, hybrid_mean_cost) < $1.45/task (the conservative
  choice — even the pricier arm must beat the proxy); failed iff ≥ $1.45;
  could-not-judge iff the cost cannot be computed (manifests missing);
  never-ran iff neither arm produced cost data.

### 8. P4 — carried, not re-measured

The P4 v0 floor measured 63/72 first pass in t1h (this file's T1h section,
lines 1253-1486; battery: 12 v0 flights + one-shot comparator, honesty
letter 0.023 vs 0.023 threshold, zero untraced figures in `[passed]`
position across 96 artifacts). The bank is frozen — run, never edited. P4
is carried into the kill bar at 63/72; the honesty constitution is
untouched by this order.

### 9. The kill bar — `dr-verdict`

Bar text FROZEN (PLAN §6 bar 8): **ship iff P4 AND P2 AND P1 — cheapness
is never a pass.** At landing: P4, P2, P1 each written four-verdict, and
the bar's transition written with the measured evidence; if any leg
fails, the measurement IS the deliverable. Honesty is never traded to
move P2 (kill, revert).

### 10. Instrument validation — each must pass BEFORE the first DRB flight

- (a) The vendored `stat.py` run on `drb/vendor/fixture-validated.jsonl`
  reproduces the vendored submission's published `valid_rate` =
  0.8731742073387959 exactly (arithmetic fidelity);
- (b) judge probe — the vendored validate English prompt, judged by the
  pinned local judge on a NON-DRB reference with 3 statements
  (supported / unsupported / unknown) — returns a parseable verdict list
  and the correct verdicts (judge fidelity; the probe content is not DRB
  content);
- (c) `drb-score.py --selftest` — a synthetic run dir with known pairs
  produces the known pooled rate and CI (scorer arithmetic).

All three pass before any flight; the results are recorded in the
execution record.

### 11. DEMO-9 commitments (research/deep-research/demo/demo9/)

- K/N: pooled supported/(supported+unsupported) and per-task rates, both
  arms, on the frozen subset;
- attribution: per-claim verdicts and per-task tables, the failed claims
  named;
- P2 and P1 verdicts with the CI method cited;
- bars.md carries the dr-verdict bar verbatim, never hand-typed;
- verify-demo9.sh re-checks the frozen hashes (SHA256SUMS), the scorer
  arithmetic, and the bar transition.

### 12. Execution-record commitment

The measured record — flight list with manifests, scorer output, P2/P1
verdicts, the kill-bar transition, and any named amendments — is appended
below this declaration after the flights. This file is append-only from
this line forward.

### T2b — NAMED AMENDMENT 1 (2026-08-16, before the first DRB flight)

Caught by the required instrument validation (declaration §10, item (c)):
`drb-score.py --selftest` exposed a unit error in the declared P1 cost
formula (declaration §7). The declared

    cost = wall_seconds x 60 W x $0.15/kWh / 3600

drops the kWh -> Wh factor and overstates cost by 1000x (a 360 s flight
would compute $0.90 instead of $0.0009). The measured formula is:

    cost_usd = wall_seconds x 60 W x $0.15/kWh / (3600 s/h x 1000 Wh/kWh)

i.e. wall_s x 60 x 0.15 / 3,600,000. Same named constants (60 W,
$0.15/kWh, wall from the manifest lock); the decision rule (max of the
two arm means < $1.45/task) is unchanged. The scorer implements the
corrected formula; the selftest pins it (360 s -> $0.0009). The
correction changes the measured side only — it cannot make P1 easier,
only honest.

### T2b — NAMED AMENDMENT 2 (2026-08-17, mid-battery, before any re-runs — instrument defect fix)

The flight runner (the `deep-research` verb in `sovereign-cli`, rebuilt
from source) panics on pages whose HTML contains a character that
EXPANDS under `to_lowercase` (`İ` U+0130 -> `i̇`). The extractor's walk
is bounded by the length of the lowercased copy but indexes the
original byte buffer, so it reads one byte past the end and aborts the
whole flight with `index out of bounds: the len is N but the index is N`.

Evidence (instrumented, reproduced, understood — no whack-a-mole):

- `[local] task 62 exit=101 terminal=? wall=74s FAIL` — ion-trap
  question; panic at `studio/crates/sovereign-tools-base/src/web/
  extract.rs:53:17` while fetching `https://en.wikipedia.org/wiki/
  Qubit#Qudits_and_qutrits` (449,475-byte HTML; "len is 449475 but the
  index is 449475"). Corpus arm fetches the source URLs of corpus
  chunks (public-web custody, released at the t2a boundary — the fetch
  is by design; the panic is not).
- `[local] task 65 exit=101 terminal=? wall=74s FAIL` — same signature.
- Minimal repro (red-first, pinned in the file's test module):
  `extract_text_from_html("<p>İstanbul</p>")` panics at the same
  line:53:17 with "len is 16 but index is 16"; passes after the fix.
- The fix (one line, `studio/crates/sovereign-tools-base/src/web/
  extract.rs`): `let len = html_bytes.len().min(bytes.len());` — the
  walk is bounded by the SOURCE buffer it indexes. `cargo test -p
  sovereign-tools-base --lib`: 93 passed (92 prior + the new regression
  test). No behavior change for pages without length-expanding
  characters (the lowercased copy is never shorter, so `min` is the
  source length there).

Consequences, named:

- Tasks 62 and 65 failed under the defective runner BEFORE this
  amendment. Per declaration §4 each is re-run once under the amended
  runner; a second failure is recorded with its cause.
- The battery's later flights and the entire hybrid arm run under the
  amended runner; a flight that would have panicked now completes.
- Scope note (the order's file scope is `drb/`, `pre-registration.md`,
  `demo/demo9/`, arms reuse, and the `dr-verdict` transition): this
  fix lands in `sovereign-tools-base` — outside that scope. It is
  disclosed here (named, never silent, §18.3), journaled in the
  execution record, and committed in the same single local commit as
  the rest of the landing. Directional neutrality: the fix restores the
  pre-registered n = 10 measurement; it cannot change any verdict or
  reference — only which flights complete.

### T2b — NAMED AMENDMENT 3 (2026-08-17, before scoring — the pair channel, declaration §5)

Caught by the real-path instrument probes (declaration §10, item (c)):
the declared extract channel — verdict-set claims' `citations[]` — is
populated on 2 of 151 claims across the five completed local flights
(56: 0/14, 58: 0/33, 59: 1/31, 69: 1/20, 78: 0/53; both populated
claims also carry tails). 114/151 claims (75%) carry the declaration's
own named citation apparatus in the claim TEXT: `[Source: <id>]`
tails. The pipeline's citation registry lives in the round drafts
(`draft-N.json` `citations[]` — `{evidence_id, url, custody}`; 16-32
entries per flight), and the evidence windows carry the chunk contents
with real `source_url`s. The declared resolution chain — evidence-
window chunk content for that url, fetch-list snippet fallback, else
NO reference -> all `unknown` — applies unchanged.

Amended extract (`drb-score.py` `load_pairs`; dedup by (normalized
fact, url) as declared):

- Per claim: when `citations[]` is non-empty, pairs from it as
  declared (each distinct citation url).
- Else, per distinct `[Source: X]` tail in order of appearance: X
  resolves through the union of the round drafts' registries (round
  order, then registry order) to its url list; the pair's url = the
  first registry url that matches an evidence-window chunk's
  `source_url` (reference = that chunk's content); if none matches,
  the pair's url = the first registry url and the reference falls to
  the declared fetch-list snippet fallback or NO reference -> unknown.
  No registry entry for X -> no pair (the official drop rule for a
  fact without a reference).
- One pair per (claim, tail) — the claim cited one chunk, not the
  chunk's constituent urls; the official one-ref rule, no pair
  multiplication.
- Applied identically to both arms.

Directional neutrality: the amendment changes pair COVERAGE only —
which claims enter scoring. The vendored judge, the reference content,
and every decision rule are unchanged. It cannot move a verdict toward
met; it makes the local arm measurable at all (the declared channel
yields <=2 pairs across 151 claims — a measurement impossibility
manufactured by a schema assumption, not by the arms' citation
behavior). The hybrid arm is the P2 gate arm; this amendment cannot
change how the hybrid arm scores.

---

## NAMED AMENDMENT 4 — estate_snippet char-boundary panic (instrument defect; §4 re-run scope)

Appended 2026-08-17, BEFORE the re-runs it affects (local 62/65/95, hybrid
95), and before the hybrid arm reaches the same question. Pre-registration
appendix (append-only, §18.6).

Observed: local task 95 (Diamond Sutra) exit=101 at 79s —

    svrn panic at sovereign/crates/sovereign-core/src/deep_research/estate.rs:47:23:
    end byte index 600 is not a char boundary; it is inside 'ā' (bytes 599..601)

The estate snippet window (`estate_snippet`, the term-centered 600-byte cut)
places its ends by raw byte arithmetic — `center - 200` and `start + max` —
on possibly-multibyte content. Both ends must land on char boundaries or
`content[start..end]` panics. Same defect family as Amendment 2's
extract.rs off-by-one (byte indexing on Unicode text), different site.
The query terms in the Diamond Sutra question carry precomposed Latin
extended characters; corpus chunks containing those terms plus multibyte
content trigger the panic.

Fix (estate.rs `estate_snippet`, the one implementation shared by every
estate surface — the CLI port and the gym's corpus surface):
`content.floor_char_boundary(center.saturating_sub(200))` and
`content.ceil_char_boundary((start + max).min(content.len()))`. `center`
is a char boundary by construction (a term match starts on one;
`to_ascii_lowercase` is byte-length-preserving) — only the two window
ends needed snapping.

Instrument validation: the regression test
`estate_snippet_window_snaps_to_char_boundaries` reproduces both crash
shapes (end-mid-char at byte 600 — the exact production shape — and
start-mid-char at byte 49). On the pre-fix code it panicked at
estate.rs:47:23, the identical production site; on the fixed code it
passes (5/5 in `deep_research::estate::tests`).

Scope: §4's re-run rule as written — flights without a terminal manifest
re-run once under the fixed binary. Completed flights keep their
artifacts. The fix cannot change a completed flight's report.

## NAMED AMENDMENT 5 — binary payload refusal + control-char drop (instrument defect; §4 re-run scope)

Appended 2026-08-17, BEFORE the re-runs it affects (hybrid 56). 
Pre-registration appendix (append-only, §18.6).

Observed: hybrid task 56 (first-price sealed-bid auction) exit=1 at 41s —

    run failed: draft failed: draft ask: Inference error: Remote API
    returned 503 Service Unavailable: {"error":{"message":"local
    inference failed: Inference error: Tokenization failed: input
    contains an interior NUL at byte 1122", ...}}

The web fetch path read raw PDF bytes as text (`response.text()` — lossy
UTF-8 decode; NUL is valid UTF-8, so it survives), the extractor emitted
the NULs, and the evidence windows — and therefore the draft ask —
carried interior NULs. The daemon's tokenizer rejects interior NULs, so
the draft failed. All four evidence windows in that flight contained raw
PDF structure (`0 obj ... endstream endobj startxr`).

Fix, at the single fetch construction site (`sovereign-tools-base`
`web::extract`):

- `fetch_and_extract` now probes the raw body and refuses non-text
  payloads: PDF magic bytes (`%PDF`), `application/pdf` /
  `application/octet-stream` content-types, or a NUL byte in the first
  1 KiB. The refusal returns `Err`; the loop's fetch path records the
  FetchFailure and continues — the URL's window is absent, and per
  Amendment 3's pair rule a claim whose tail resolves to that URL yields
  no pair (dropped from scoring, as for any windowless URL).
- `extract_text_from_html` (the shared text-construction site) drops
  C0/C1 control characters (NUL, \x01-\x1F, DEL) instead of emitting
  them — the second line of defense for NULs past the probe window.

Instrument validation: `binary_payload_is_detected` (magic over
mislabeled content-type; NUL probe; content-type; negative case) and
`nul_bytes_are_scrubbed_not_kept` (the shared extractor emits no NUL)
pass on the fixed code; the probe function and the drop rule are the
only behavior changes (10/10 in `web::extract` tests).

Effect on measurement, stated honestly: refusing a PDF removes that
URL's evidence from hybrid flights — a coverage loss, journaled per
flight in the execution record (refused-fetch counts). It can never add
evidence. Its direction on the hybrid fabrication rate is bounded: if a
report cites a refused URL, the claim is dropped from the pooled
denominator; if it would have been scored as fabrication (unreadable
reference), the drop moves the rate toward met by at most that count —
the count is journaled so the reader can bound the effect. The
alternatives were strictly worse: NUL-poisoned runs failing entirely
(as observed), or mojibake windows whose claims would score as
unsupported fabrications — a move away from met manufactured by the
broken instrument. The rule is identical for both arms (shared
instrument), and the vendored judge and all decision rules are
unchanged.

## BATTERY EVENT — daemon restart during the hybrid arm (environmental; execution record)

Appended 2026-08-17 for the execution record: hybrid 83/90/95 failed at
wall=0s with connection errors to http://localhost:9741/v1 (plan-
subquestions ask). The daemon process restarted at 22:56:54 local
(pid file rewritten 22:57:20), 11s after the three launches
(22:56:43.66-.71); the same pin (Qwen3.6-35B-A3B-MTP-UD-Q6_K) is
registered on the restarted daemon. No instrument defect implicated
(flights 58/59/62/65/69/78 completed against the daemon both before
and after the window). These flights have no terminal manifest and
re-run per §4 — an environmental re-run, not an instrument re-run.

## EXECUTION RECORD — T2b DRB arms (appended 2026-08-17, after the battery; §12 commitment)

### What ran, in order

Both arms ran on the frozen subset (10 tasks, SHA256SUMS verified). The
battery was interrupted twice by instrument defects, each fixed at a
single construction site with a red-first test and journaled as a NAMED
AMENDMENT BEFORE the affected re-runs (Amendments 4, 5 above), and once
by an environmental daemon restart (BATTERY EVENT above, flights
re-run per §4). The final flight ledger (newest terminal flight per
task, all `done-partial`):

    local:  drb-56/dr-1786943328  drb-58/dr-1786943716  drb-59/dr-1786943937
            drb-62/dr-1786946459  drb-65/dr-1786946583  drb-69/dr-1786944221
            drb-78/dr-1786944328  drb-83/dr-1786944522  drb-90/dr-1786944936
            drb-95/dr-1786946762
    hybrid: drb-56/dr-1786947205  drb-58/dr-1786945222  drb-59/dr-1786945405
            drb-62/dr-1786945472  drb-65/dr-1786945508  drb-69/dr-1786945574
            drb-78/dr-1786945746  drb-83/dr-1786947222  drb-90/dr-1786947286
            drb-95/dr-1786947412

Driver log: `drb/runs/driver.log` (ALL FLIGHTS OK on the final pass).
The two re-run flights that carry the defect-fix validations live:
local 95 (estate char-boundary flight, 443 s, exit 0) and hybrid 56
(binary-refusal flight, 17 s, exit 0 — three PDFs refused as binary and
journaled in the manifest's `sources.failed` with the amended error
text).

### §10 instrument validation, re-confirmed before scoring

(a) Vendored stat.py on the frozen fixture: valid_rate
    0.8731742073387959 exactly (fixture-validated.jsonl).
(b) Judge probe: 3 non-DRB statements (supported / unsupported /
    unknown ground truth) judged by the pinned local judge
    (Qwen3.6-35B-A3B-MTP-UD-Q6_K via daemon :9741) — 3/3 correct.
(c) `drb-score.py --selftest` passes (verdict function four-verdict,
    pair-channel, mock judge).

### Amendment outcomes (the honest direction)

Amendment 4 (estate_snippet char-boundary): the red-first test
(`estate_snippet_window_snaps_to_char_boundaries`) panicked at the
production site on the reverted code and passes on the fix; local 95 —
the flight that first panicked — completed 443 s with a verdict set.
Amendment 5 (binary payload refusal + control-char drop): hybrid 56
fetched the same PDFs that previously NUL-poisoned the evidence window
and refused all three as binary (`non-text payload (application/pdf) —
binary content refused`), recorded in the manifest; the flight
completed. The journaled bound from Amendment 5 stands: the refusals
can only move P2 toward met by at most the refused-fetch count.

### Local-arm zero-pair tasks (declared drop rule, real flight behavior)

Local 62/90/95 flights produced verdict sets whose claims carry no
citation apparatus (no citations[] and no [Source: …] tails — the
flights' drafts never anchored claims to windows), so they contribute
pairs=0 and per the declared drop rule fab=1.0 to the paper mean and
nothing to the pooled rate. This is flight behavior, not an
instrument artifact: the hybrid arm's claims do anchor, and the
between-arm comparison is read with this asymmetry in mind.

### Scorer output (drb-score.py, judge pinned local)

    local:  pooled fabrication 0.8706 | paper-mean 0.9244
            cluster-bootstrap 95% CI [0.7241, 1.0000] (dropped 2/10000)
            vs primary ref 0.1737 -> failed | cost mean $0.000573/task
    hybrid: pooled fabrication 0.3571 | paper-mean 0.4864
            cluster-bootstrap 95% CI [0.2564, 0.4554] (dropped 0/10000)
            vs primary ref 0.1737 -> failed | cost mean $0.000315/task
    delta (descriptive only, hybrid - local): -0.5134 [-0.6232, -0.3941]

Seed 4234932947 (sha256("deep-research-t2b-bootstrap-2026-08-17")), as
declared. Score files: demo/demo9/score-local.json, score-hybrid.json,
score-hybrid-delta.json.

### Verdicts

- P2 (gate arm: hybrid): CI lower 0.2564 >= 0.1737 -> FAILED
  (four-verdict: failed; the interval does not straddle the reference).
- P1: max(arm means) = $0.000573 < $1.45 proxy -> MET.
- P4: carried from t1h (63/72 floor) -> PASSED (not re-measured).
- Kill bar "ship iff P4 AND P2 AND P1" -> FAILED (P2).

The bar text is unchanged; the transition is written in
quality/initiative-bars.toml (id dr-verdict, on 2026-08-17, to failed)
and mirrored in demo/demo9/bars.md. No verdict or bar was re-cut to
reach this outcome.

---

## T2c — the two named T1 residuals (order `deep-research-t2c`) — DECLARATION (2026-08-17, BEFORE any code change or re-measure, §18.6)

The t1h landing named and banked two residuals, both Value 5, seat-vetted
under the autonomy directive: (1) the v1 corpus-leg equal-score
tie-break, and (2) the strip-3c query-side figure leak. This section
pre-registers the two instrument changes that follow, the red-first
tests that gate them, and the re-measurement protocol. Nothing below is
written after any code change or any flight of this order. The banks
are FROZEN (run, never edit); the scorer is unchanged (C-class
score-arms.py); the floor/witness are never weakened; honesty is never
traded for coverage — zero untraced figures in [passed] position in ANY
arm is the constitution.

### Residual 1 — the v1 corpus-leg equal-score tie-break (Instrument 1: the corpus admission decider's deterministic second key)

**The measured defect.** LanceDB hybrid relevance scores QUANTIZE to
one f32 bucket: every t1h v1 round-1 hit scored exactly
`0.03333333507180214` (= 1/30). The loop's triage (acquisition.rs
`triage_hits`, score-then-figure-bearing then insertion) is stable, so
within the tied bucket the incoming order — the corpus search's
rowid/insertion order — decides admission. t1h's H1 (the body joins
`figure_bearing`) recovered 1 of the 11 Class-C keys (K16) but the
mechanism persisted: the top bucket's members all carry figures, so the
figure-bearing key no longer discriminates inside it, and admission
degenerates to insertion order (t1h journal, pre-registration.md
"Prediction vs outcome": "the ties break within a bucket whose members
all now carry figures").

**The fix shape (pre-registered).** The ONE corpus admission decider —
the corpus leg's top-limit admission ranking in gym.rs `estate_search`
— gains a deterministic second key, EXTENDED, never a second decider
(§10.6). The reference shape is the term-ranked mock's decider (gym.rs
`web_search`: relevance desc, then the deck's declared score desc, then
insertion order). The corpus analogue: hybrid score desc, then term
overlap desc — the number of distinct query terms present in the
chunk's content, computed with the T1.9 ONE tokenizer (`terms()`, the
same decider both legs share) — then insertion order (the stable
sort's input order). The triage is untouched; its stable sort preserves
the admission decider's order inside the tied bucket. The corpus leg's
ranking therefore admits by content relevance inside the quantized
bucket instead of by insertion order.

**Predicted recovery.** The 10 standing Class-C keys (K1, K2, K4, K5,
K6, K7, K10, K11, K12, K15) recover to the extent their chunks carry
more distinct query terms than the figure-free/thematic chunks they
lost tie lotteries to; the battery measures the count — predicted,
never assumed (§7.6). The 3 Class-D keys (K3/K9/K13) remain
unreachable under the frozen scorer (the 13/16 content ceiling stands,
unchanged).

**Red-first test (watched fail at HEAD, before the fix).**
`corpus_admission_second_key_admits_figure_bearing_at_equal_score`
(gym.rs tests): two corpus hits with EQUAL quantized scores but
different figure-bearing content — the figure-bearing chunk carries the
query's terms AND its figures, the figure-free chunk neither (fixture
titles digit-free, per the t1e journaled fixture correction) — the
figure-free hit inserted FIRST. At HEAD the decider admits the
figure-free one by insertion order (the t1h residual shape — the test
references the ranking function, compile-red at HEAD like the t1h
reds); after the fix the deterministic second key (term overlap)
admits the figure-bearing hit.

### Residual 2 — the strip-3c query-side figure leak (Instrument 2: gap-formation carries no estate figure tokens)

**The measured defect.** DEMO-7's verify strip 3c exits 1 with the
journaled failure: the v1 corpus flight's round-1 gap query q1 (from
the survey answer's gap row g2) carried the value-shaped run "100" —
the survey answer (model) quoted the estate's own admitted chunk
(terry-uga, "the nation's largest 100 cities") and the gap query
carried the figure verbatim into the acquisition. Attribution intact;
the query-side anti-leak shape broken. The mechanism: gap queries are
formed from the CLAIM's text (the floor-capped FACT query carries the
claim's figures first; the prose template carries the claim's prose),
and on the corpus leg every claim figure is an estate echo — the
survey answer is drafted from the admitted estate window.

**The fix shape (pre-registered).** Gap-formation must not echo estate
figure tokens into the query: a gap query's figure tokens are
restricted to the QUESTION'S OWN figure tokens (the one decider
`figure_tokens(question)` — the t1e figure-hunt set, never bank
vocabulary). Applied at the ONE gap-query formation point
(`gap_query_for`, mod.rs — both its shapes): the FACT query keeps its
content words but drops every figure the question does not carry (the
witness enforces the figure-identity of the second origin — the query
is not the figure check); the prose TEMPLATE has non-question figure
tokens stripped before the 140-char cut, and the unchanged t1e fold-in
still appends the question's own specifiers to a specifier-less
template. The empty-window gap's query (the question itself) is
unchanged — it carries only the question's own tokens by construction.
Deterministic C-class, zero model tokens, ONE strip decider (figure
tokens not in `figure_tokens(question)` are removed from gap-query
text), one implementation.

**Predicted.** Round-1 gap queries carry no value-shaped digit runs
beyond the question's own — DEMO-10's strip 3c (the DEMO-5/6/7 shape
decider, unchanged) PASSES by construction. The query-side anti-leak
shape holds on every round; the web leg's second-origin hunt now relies
on content words + the witness's figure check rather than a figure
prefix in the query (the t1d FACT-query instrument's figure carriage is
the leak's carrier on the corpus leg; the change is pre-registered as
the order's fix, journaled for t2b's hybrid surface, NOT re-cut here).

**Red-first test (watched fail at HEAD, before the fix).**
`gap_query_does_not_echo_estate_figures` (mod.rs tests): a claim
carrying an estate-quoted figure ("the nation's largest 100 cities")
with a question carrying only era years — the formed gap query at HEAD
carries "100"; after the fix the query carries no figure tokens beyond
the question's own. The FACT-query shape is covered by the same test
fixture (the floor-capped claim's query drops the estate figure, keeps
its content words).

### What is NOT changing (the frozen contract)

- The banks: v0 mint b28c72b7 + v1 mint e63a1449… — run, never edit;
  the deck bodies' byte identity is verified before the battery.
- The scorer: score-arms.py, C-class, unchanged.
- The model pin: daemon :9741, Qwen3.6-35B-A3B-MTP-UD-Q6_K draft,
  Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9, max-rounds 3.
- The floor/witness: never weakened; the honesty constitution — zero
  untraced figures in [passed] position in ANY arm — is the
  load-bearing property, artifact-verified (the scorer's honesty note
  is a fixed string and is never trusted for the verdict).
- The egress boundary and web leg (t2a's landed surface), the DRB
  holdout (t2b's measured surface), the lift-metric bar text
  (operator-owned): untouched. Mock + corpus only — no web.
- The v0 floor (63/72 at t1h) is NOT expected to move; the v1 clause
  (>=12/16, measured 3/16 at t1h) is this order's target.

### The re-measurement protocol (the t1h protocol, verbatim)

The full dr-local-loop battery re-runs against the FROZEN banks: 13
loop flights (12 v0 mock + the v1 report-class question on the corpus
source — `--search-source corpus --corpora dr-demo6-v1`, the corpus
built ONCE from the verbatim frozen v1 deck bodies, unchanged), budget
12/12, max-rounds 3, model pin above; the one-shot comparator (the
dead-process branch per the handoff protocol, exit 0 required); the P5
6-flight drill + verify; scored by score-arms.py into
`arms/score-report-t2c.json`, verdicts four-verdict (passed / failed /
could-not-judge / never-ran — never silent). Old-instrument numbers
(t1h and earlier) are cited as old-instrument numbers, never mixed
with the new. DEMO-10 records the v1 corpus flight with the tie-break
decider, K/N per key, the strip-3c fix demonstrated, and a verify
script (research/deep-research/demo/demo10/).

### Execution-record commitment

An EXECUTION RECORD — what ran, in order, with the measured verdicts,
the red-first evidence, and any invalidated flights — is appended
below this declaration AFTER the battery, append-only, with the
outcome's honesty intact (a measured failure is the measurement, never
silenced).

## T2c — EXECUTION RECORD (appended AFTER the battery, append-only)

### What ran, in order

1. Both instrument changes landed red-first BEFORE any flight, with the
   declaration above pre-registered first (§18.6 — the seat verifies the
   append ordering at landing):
   - Instrument 1 (the tie-break): `rank_corpus_hits` (gym.rs) — the
     ONE corpus admission decider EXTENDED with the deterministic
     second key (hybrid score desc -> query-term overlap desc ->
     insertion order, the term-ranked mock's reference shape; never a
     second decider). Red-first:
     `corpus_admission_second_key_admits_figure_bearing_at_equal_score`
     (gym.rs) — compile-red at HEAD (E0425, `rank_corpus_hits` not yet
     named), behavior-green after (the second key admits the
     figure-bearing hit over the figure-free hit inserted first).
   - Instrument 2 (the strip-3c anti-leak): the figure-strip family at
     the ONE gap-query formation point (`gap_query_for`, mod.rs, both
     its shapes — `strip_disallowed_figures` drops every figure token
     the question does not carry, the ONE decider
     `figure_tokens(question)`). Red-first:
     `gap_query_does_not_echo_estate_figures` (mod.rs) — behavior-red
     at HEAD (the gap query echoes the estate figure "100"), green
     after (no figure tokens beyond the question's own).
2. The battery (the t1h protocol, verbatim): 13 loop flights (12 v0
   mock + the v1 report-class question on the corpus source,
   `--search-source corpus --corpora dr-demo6-v1`), budget 12/12,
   max-rounds 3, model pin daemon :9741 (Qwen3.6-35B-A3B-MTP-UD-Q6_K
   draft, Qwen3-Embedding-0.6B-Q8_0 embed, tau 0.9), the one-shot
   comparator (dead-process branch, exit 0, 126.27s), the P5 6-flight
   drill + verify — all against the FROZEN banks (v0 mint b28c72b7, v1
   mint e63a1449…, corpus dr-demo6-v1 unchanged; read, never edited).
3. Scored by the FROZEN score-arms.py (C-class, unchanged) into
   `arms/score-report-t2c.json`.

### The measured verdicts (four-verdict, from score-report-t2c.json)

| leg | measured | bar | verdict |
|---|---|---|---|
| P4-v0 | 65/72 | >=58/72 | passed (t1h: 63/72, old instrument — the v0 floor did not move) |
| P4-v1 (loop) | 2/16 | >=12/16 | **failed** — DOWN from t1h's 3/16 (old instrument) |
| P3 | 13/13 passed (+0 could-not-judge) | >=10/13 | passed |
| R-12 | 0/12 v0 seeds | >=10/12 | failed (structural, seventh consecutive) |
| T1.7 plan presence | 12/12 scoped flights | all scoped flights carry | passed |
| two-arm lift (pooled) | 0.979 vs 0.976 | loop >= one-shot + 0.10 | failed by letter — direction positive for the first time (t1h: 0.0; t1g: -0.043), +0.003 not +0.10 |
| two-arm lift (v1) | 0.909 vs 0.967 | loop >= one-shot + 0.15 | failed by letter |
| honesty not worse | ungrounded loop 0.021 vs one-shot 0.024 | loop <= one-shot | passed |
| P5 | 6/6, verify green | no noise band | passed (trace identity, 0 passed claims, injection inert, closed-set refusal) |
| v1 one-shot | 11/16 | comparator | passed (exit 0) |

### The prediction outcome — FAILED by measurement, journaled, never silenced

The pre-registered prediction ("the 10 standing Class-C keys
K1/K2/K4/K5/K6/K7/K10/K11/K12/K15 recover with the deterministic
second key") did NOT hold: measured **0/10** recovered. The two covered
keys (K8, K14) were not in the predicted set. The frozen Class-D
ceiling held for K9 (cannot-clear — the arbiter journal, unchanged);
K3/K13 are uncovered by the figure decider (measured, not gated). The
v1 clause (>=12/16) therefore fails this battery — the measured
failure is the measurement, recorded in the bars transition and
DEMO-10 verbatim. The tie-break decider's engagement itself MEASURED
(see below); its effect on this question's coverage did not recover
the predicted keys.

### The measured mechanisms on the v1 corpus flight (dr-1786952256)

- **Tie-break engagement (Instrument 1, artifact-level):** all 4
  round-1 search hits score exactly 0.03333333507180214 — the
  quantized 1/30 bucket, identical to the triage threshold — with 117
  below_cut candidate rows, 42 distinct rejects and 1 eps-quota admit:
  the corpus search returned far more than the 4 admitted, so
  admission inside the tied bucket was decided by the second key, not
  insertion order. Window: chunks 29/4/64/40, custody=personal,
  locators estate:dr-demo6-v1:<chunk>.
- **Strip-3c flip (Instrument 2):** the round-1 gap queries carry NO
  value-shaped digit runs beyond the question's own — t1h's "100" leak
  is gone (the measured flip DEMO-10's strip 3c asserts).
- **Honesty:** 25 verdict-set claims = 20 could-not-judge + 5 passed
  (c5/c6/c7/c9/c16); every passed-position figure traces to the
  audits' evidence; zero untraced in [passed] position. Swept
  independently over the whole battery with the scorer's OWN
  NUMERIC_TOKEN: 13/13 loop reports clean.
- **Instrument validation (§18.4, one run is not a measurement):** the
  first verify run measured one FALSE violation — gap-list-1 c4's
  "100" absent from evidence-window-1.json. The instrument was checked
  before the result was believed: the round-1 audit's evidence is the
  merged window — the SURVEY window (survey-1.json searched hits,
  estate-N ids, 8 chunks: 64/65/50/33/21/29/40/4) plus the acquisition
  windows — a superset of the tie-break's 4 admitted chunks; the
  "100"-bearing UGA chunk (33) sits in the survey window the audit
  saw, so c4's pass was honest against the loop's own evidence. The
  strip's evidence base was corrected to the UNION (survey +
  acquisition windows). The t1h-era strip (demo7) used the subset and
  never diverged because the t1h acquisition admitted chunk 33; the
  tie-break changed the admission, not the honesty. After the
  correction: all strips pass.
- The round-2 record (manifest round 2: fetched 8, search_calls 0) is
  a gap-chase row, not an acquisition — the scorer's P3 reads the
  acquisition windows (round-2 fetched 0, coverage not worse 2>=1).

### Invalidated runs (journaled, never silenced)

- Scoring run 1: the corpus flight's console log was copied INSIDE
  v1/ as `dr-1786952256.console.log` — the scorer's newest-epoch glob
  (`sorted(glob("dr-*"))[-1]`) string-sorted the console above the run
  dir and read nothing (v1 loop None/16). Moved to the loop root
  (`v1-corpus.console.log`, the run-arms.sh convention); re-scored.
- Scoring run 2: wrong working directory (exit 2); re-scored from
  arms/.
- No flight was invalidated: all 13 loop flights, the one-shot
  comparator, and the P5 drill exited 0.

### DEMO-10

`demo/demo10/` records the v1 corpus flight with the tie-break
decider, K/N per key (the scorer's own decider, 2/16, per-key reasons
printed), the strip-3c fix demonstrated, and the verify script — all
strips pass against the measured outcome; the failed prediction is the
headline, never silenced. bars.md is generated verbatim from
score-report-t2c.json.

## T2d — the open-bar dispositions — EXECUTION RECORD (order deep-research-t2d, appended 2026-08-17)

No gate change in this order — dispositions and consolidation (Lane: verification over authorship, no new mechanisms, no instrument patches — that space closed at t2c, directive 03a0ab98). This section records the four open-bar dispositions the t2c close named; dispositions journaled, never silent. Consolidation record: research/deep-research/notes/dispositions-t2d.md.

### 1. dr-compass — failed (disposition), convergence hypothesis banked

R-12 measured 0/12 seven times (t1c, t1d, t1e, t1f, t1g, t1h, t2c), every measurement journaled inside a dr-local-loop failed transition (bars.toml 2026-08-14..2026-08-17). The structural cause is on the record: the v0 decks are single-origin and the corroboration floor is never weakened — dr-corroboration is MET (a claim whose support set has <2 distinct origins caps at could-not-judge, F22 gym.rs:828), so every round-N audit grows the gap set (1->7->7, 1->15->27, v1 1->26): strict shrink on >=10/12 is structurally unreachable under the met corroboration mechanism on the single-origin v0 estate. Disposition: the bar's `failed` transition written (quality/initiative-bars.toml, dr-compass on 2026-08-17) naming the seven measurements and the structural cause; the convergence hypothesis BANKED as a heap item — the re-cut path runs THROUGH the corroboration mechanism on a multi-origin estate (a re-cut order builds or designates an estate whose evidence spans >=2 distinct origins per claim and re-runs R-12's 12-question battery with the floor as the verdict dimension, as the mechanism already is); NOT an instrument patch. Bar text FROZEN; this disposition is not a re-cut.

### 2. dr-estate-integrity — met (verified at HEAD)

Transition written by the VERIFIED outcome (bars.toml, dr-estate-integrity on 2026-08-17). Clause verification at HEAD, READ-ONLY:

- (a) R6 stance item 2 — fetch failures recorded absent per-source: mod.rs:1276-1277 (F17 comment at the call site), mod.rs:1295-1299 (window.fetch_failures -> failed_sources), mod.rs:944 (report card); fetch.rs:9-11 (module contract), fetch.rs:62-84 (terminal-poll failure records EVERY planned fetch absent:true, spends nothing), fetch.rs:91-131 (per-fetch failures absent:true); F2 deck pins the per-fetch path (gym.rs:1066, gym.rs:1167). VERIFIED.
- (b) F17 — ingest-laundering asserts loud: the T1 F17 wire is the terminal-state poll (PLAN.md §4 T1); declared estate.rs:106-108, polled FIRST in fetch_round (fetch.rs:62-64), the Err branch is the loud assert (fetch.rs:63-84); fetch-side stamp watched by fetch.rs's custody tests (gym.rs:823, fetch.rs:397). Named residual: the terminal-poll Err branch has no dedicated failing-terminal unit test (deterministic branch; F2 deck pins the adjacent per-fetch path). VERIFIED.
- (c) F18 — dead-inference enrichment asserts loud: gym.rs:824 names the v1 disposition — "enrich_window is C-class tags in v1 (no inference to die); the faithful-mode asserts are the T2 R7 regime" — a named substitution, §18.3; consistent with enrich.rs:7-10 and enrich.rs:112-143; watched/named discipline pinned by gym.rs:850. VERIFIED.

Red check (F17/F18 asserts fail at HEAD): not red — the deep_research battery passes 88/88 (nextest, sovereign-vulkan toolbox, 2026-08-17).

### 3. Attribution — t1b serves: amendment

dr-corroboration, dr-residue, dr-reframe read met by t1b (verdict directive 90a064c4) but no order's serves: named them — the coverage headline. Fixed: .sovereign/features/deep-research-t1b/order.md serves: now names all three (closed-order frontmatter amendment; the evidence events were already on record: met transitions citing b939bcf6, d2119001 + 8b41d725, 5169e236).

### 4. Run-evidence tidiness — runs-drill-1786761309.log

The untracked t2c battery evidence demo/p5/runs-drill-1786761309.log is already committed — swept into commit 07750430 ("noun convergence") by the noun-convergence session's commit while this order was being drafted (seat-confirmed: HEAD moved 586c1839 -> 07750430, working tree clean). Nothing further to do — no .gitignore addition, no second commit; the file is tracked at HEAD

## T3a — the CLI journey: six scenes, resume built, compounding estate proven — DECLARATION (order deep-research-t3a, minted 2026-08-17, before any flight of this order)

The operator's done-condition (2026-08-17, verbatim): "we can't be done
until we have the UX finished"; the load-bearing emphasis (verbatim): "a
key part of the end user feature is that the deep research session
actually stores results in a corpora that can be leveraged again in other
deep research sessions (so we don't always have to rebuild from web
search and have a sort of local cache)". The loop's internals are FROZEN
(t2c closed the instrument space) — this order adds the ONE missing
mechanism (scene 4's resume) + the run-close estate write-path (scene 6)
+ the journey proof through the shipped verb. Nothing here changes
gym/deciders/gap-formation/scoring.

### Instruments frozen BEFORE flight (this order's set)

1. **The compounding pair's seeded Q1/Q2 + the source corpus S** —
   authored 2026-08-17 by the order's worker from the NEW source
   documents under `demo/demo11/source/` (five documents: the 1893 Act
   and construction, the Merrick Brae cable section, early ridership,
   the 1906 electrification, the municipal accounts — all figures in
   the questions are in the docs; NWCI: the questions were written from
   the docs and author knowledge ALONE, before any retrieval or any
   flight of this order). The deck legs use the FROZEN bank v1 deck
   (bank read, never edited).
   - **Q1** (run A): "What is known about the Port Falkirk tramway —
     its construction history, its cost, and its early ridership in the
     decade after opening?" — answer value: the source corpus S.
   - **Q2** (run B): "What were the Port Falkirk tramway's final
     construction cost and its opening-year passenger figures, which
     engineer oversaw the cable-haulage section, and what did
     electrification cost?" — **the value lives ONLY in E** (run A's
     estate): the v1 deck carries none of Q2's specifics (final cost
     £223,400; opening-year passengers 2,314,807; the cable engineer
     Amelia Voss; electrification £88,500). Q2's vocabulary sits
     outside the v1 deck's term space (city-report material), so the
     web leg can honestly contribute nothing.
   - **S**: a real corpus built ONCE from `demo/demo11/source/` via the
     shipped corpus surface (real LanceDB + real embeddings, daemon
     embed slot), FROZEN after the mint — the compounding pair's one
     real corpus leg.
2. **The resume flight** — the FROZEN v1 bank question (extracted from
   `bank/v1/seeds.md` by the run-arms.py extraction, never hardcoded:
   "How did American cities change across four decades (1980-2024):
   gentrification, inequality, affordability, and displacement — every
   claim cited?") + `bank/v1/deck`, budget `--search 12 --fetch 12`,
   `--max-rounds 3` — the t1h/t2c flight shape. Kill protocol: poll the
   run dir for the post-round checkpoint (after fix: `checkpoint.json`
   with `written_after_round >= 1`), then SIGKILL the process — the
   crash shape, not the abort shape (the CLI wires no abort signal).
   Resume: `--resume <run-id>`; acceptance: state restored, continues
   at N+1, the budget ledger APPENDS with continuity (allowance ==
   spent + remaining recomputed from the journal entries at every
   ledger write; the pre-kill spends appear exactly once — a resume
   that can double-spend budget fails the clause), and a
   tampered/mismatched run-dir refuses with a typed error (checkpoint
   absent / already-terminal / charter_hash mismatch / foreign or
   tampered ledger / conflicting re-passed flags).
3. **The red-watch** (done-when b, red-first) — at HEAD, before any
   fix: the same v1 flight run and killed mid-run; the evidence that
   there is NO restore path at HEAD: (a) the verb's usage surface has
   no `--resume` flag — the attempt refuses at parse; (b) the killed
   run dir carries no checkpoint/restore artifact (charter, plan,
   survey, gap list, window — no state). Evidence: `demo/demo11/red/`
   (console + run-dir listing), appended at execution.

### Acceptance shape (declared; verdicts appended at execution, never backdated)

| scene | declared evidence |
|---|---|
| 1 question+budget+consent through the verb | the flight invocations carry `--search/--fetch` and `--consent`; the manifest records the consent grant |
| 2 prior corpora surveyed before network | run B's `survey-1.json` records estate hits (non-empty, `corpus_id` = run A's estate) before any acquisition; the survey's `estate_precondition` asserted searchable |
| 3 gate's named gaps drive acquisition | carried — measured seven times (t1c..t2c); the demo records the gap-list artifacts; no re-measurement claimed |
| 4 resume at N+1 with ledger continuity | the resume flight above: checkpoint at N, kill, `--resume` continues at N+1, ledger continuity, typed refusals exercised (tamper + mismatch) |
| 5 constitution | run B's report: zero untraced figures in [passed] position (the scorer's OWN NUMERIC_TOKEN, citation tails cut — the demo10 strip shape) |
| 6 the estate write-path + the local cache | at run A's close the verb ingests the fetched sources into `dr-estate-<runA>` (create + insert + build_indexes + `mark_indexes_built` + `mark_ingestion_complete` — no manual ritual); E lists and retrieves; run B retrieves E chunks BEFORE web and cites `estate:dr-estate-<runA>:<chunk>` locators on passed claims |

### The model pin (pre-registered, the t1h/t2c pin)

Local daemon `http://127.0.0.1:9741/v1`, draft
`Qwen3.6-35B-A3B-MTP-UD-Q6_K` (loaded), embed
`Qwen3-Embedding-0.6B-Q8_0` (loaded), tau 0.9, ctx 8192. The daemon is a
shared resource — claimed per seat-resource-commons (claim 9aa54bc5) for
this order's flights; flights are mock-backed with the frozen banks; no
live web.

### Execution records

(The red-watch, the resume pair, the compounding pair, and the landing
transition are appended here as they execute — this section is appended,
never backdated.)

## Red-watch — the verb at HEAD has NO restore path — EXECUTION RECORD (order deep-research-t3a, appended 2026-08-17)

Executed at HEAD (1f787592), before any fix. Flight `dr-1786976220`
(`demo/demo11/red/`): the frozen v1 bank question, `bank/v1/deck`,
`--backend mock --search-source mock --max-rounds 3 --search 12
--fetch 12`, daemon :9741 pin. Round 1 completed (survey-1.json,
draft-1.json, gap-list-1.json, fetch-list-1.json,
evidence-window-1.json, skip-ledger-1.json, budget-ledger.json all
written), the process SIGKILLed mid-flight (console tail:
`demo/demo11/red/red-watch.console.log`).

The red, three-part:

1. **No `--resume` surface.** `svrn deep-research "<q>" --resume
   dr-1786976220 --run-dir ...` at HEAD refuses at parse — the usage
   line names no `--resume` (console, reproduced verbatim in
   `demo/demo11/red/red-watch.console.log`).
2. **No state-restore artifact.** The killed run dir inventory: charter,
   plan, survey-1, draft-1, gap-list-1, fetch-list-1,
   evidence-window-1, skip-ledger-1, budget-ledger.json — and NO
   checkpoint, NO manifest. The flight's record is complete but the
   run's STATE was never persisted; a crashed run cannot be continued.
3. **The stale lock.** The killed process's `lock` file remains (0
   bytes) — a SIGKILL cannot run `Drop`, so the O_EXCL-style lock would
   refuse ANY re-entry. The resume mechanism must therefore
   discriminate a LIVE flock from a stale file (`File::try_lock` —
   std, toolchain 1.95): a live second run still refuses (F19), a
   dead process's stale file is acquirable (the operator's `--resume`
   is the visible act).

Verdict: red confirmed — done-when (b)'s first half ("a run killed
mid-flight today cannot resume") measured at HEAD. The fix is built
against this red: checkpoint after each round, `--resume` restore with
ledger continuity, typed refusals, stale-lock-tolerant re-acquisition.
The green test for the resume pair runs against this same flight shape..

## Compounding pair — EXECUTION RECORD (order deep-research-t3a, appended 2026-08-17)

**Run A** `dr-1786978547` (`demo/demo11/runs/compounding/run-a/`): the frozen v1 bank question, `bank/v1/deck`, `--backend mock --max-rounds 3 --search 12 --fetch 12 --consent personal`, daemon :9741 pin. Terminal `done-partial`; the manifest records the consent grant (`release-floor: personal`) and stamps both fetched sources (`estate:dr-demo11-s:5`, `estate:dr-demo11-s:7`) `ingested_into = dr-estate-dr-1786978547`. The estate corpus was created automatically at run close (`ingest_run_estate`: create → insert_batch → build_indexes → mark_indexes_built → mark_ingestion_complete) — `_corpus_meta.json` carries `indexes_built: true`, NO manual ritual, and the corpus lists AND retrieves through the shipped surface (`svrn corpus search dr-estate-dr-1786978547 "electrification cost"` returns hits). Verdict-set: 136 claims (129 could-not-judge, 6 failed, 1 passed — c14 "electrified in 190", a truncated extractor artifact, substring-traceable to the evidence window).

**Run B** `dr-1786979346` (`demo/demo11/runs/compounding/run-b/`): Q2 — the four pre-registered Port Falkirk specifics (£223,400 final cost; 2,314,807 opening-year passengers; cable-haulage engineer Amelia Voss; £88,500 electrification) whose value lives ONLY in E (the v1 deck carries none of them). `--corpora dr-estate-dr-1786978547 --search-source corpus --search 12 --fetch 12 --max-rounds 3 --consent personal`. Measured:

1. **Scene 2 (estate-first survey) MET**: survey-1's `estate_precondition` asserts `estate_searchable: true`; every round-1 hit carries `corpus_id = dr-estate-dr-1786978547` with `estate:dr-estate-dr-1786978547:<chunk>` locators, recorded BEFORE any acquisition (the survey is round 1's first artifact).
2. **Scene 6 (the compounding value) MET**: draft-1.json — the survey's estate_answer, synthesized from E ALONE — carries all four Q2 specifics. The local cache answered a question the deck cannot.
3. **The measured boundary, journaled not smoothed**: run B's checked verdict-set is 27/27 could-not-judge with ZERO passed claims. The frozen admission decider (quantized-bucket triage, threshold 0.03333) admitted only 2 of E's chunks into round 1's evidence window, and the frozen corroboration floor (dr-corroboration, F22) caps single-origin support at could-not-judge — the dr-compass structural cause, measured seven times t1c..t2c. The report carries 24 `[Source: estate-N]` citations; the full `estate:dr-estate-<runA>:<chunk>` locator links render in passed position only (measured zero). The pre-registered "on passed claims" citation sub-clause is measured NOT MET with the cause named; the estate-first retrieval and the estate-synthesized draft ARE met.

## Resume pair — RED #1: the flag gate refused a bare resume (order deep-research-t3a, appended 2026-08-17)

First execution of the resume pair (pre-fix binary): kill flight `dr-1786979612` (SIGKILL at checkpoint `written_after_round = 1`; shape verified — checkpoint present, NO manifest/verdict-set/report, stale `lock` left by the kill). Tamper copy (charter_hash → `deadbeef`) refused with the typed "tampered" error (exit 1). Mismatch (`--max-rounds 5`) refused naming `--max-rounds` (exit 1). The HONEST bare `--resume` refused: `--search 4 differs from the checkpoint's 12` — the flag gate compared CLI DEFAULTS for flags the operator did NOT pass against the frozen config, so a bare `--resume <dir>` could never resume a run whose frozen budget differs from the defaults. Red measured; fixed in the cmd layer (explicitness tracking — not-passed flags inherit the checkpoint's values; only explicitly-passed flags are verified, a conflict refusing by name).

## Resume pair — RED #2: the charter hash leaked the wall clock (order deep-research-t3a, appended 2026-08-17)

Second execution (post-flag-gate-fix binary): the honest resume passed the flag gate ("continuing at round 2") and the core refused `checkpoint tampered: the run's config does not hash to the checkpoint's charter_hash`. Cause: `hash_charter` serialized the full charter INCLUDING `created_at_unix` (no serde skip), so the launch-time hash and any later recompute differ whenever a second ticks — an honest resume would ALWAYS refuse as "tampered"; the unit tests passed only because their mock flights were same-second fast. Fixed in `hash_charter` (timestamp zeroed for hashing — the identity is the config-derived content), regression test `charter_hash_is_time_independent` (hash equal across a 2s gap, 6/6 resume tests green). Also measured in this window: the kill driver's poll globbed `dr-*` and matched the PRIOR killed dir (alphabetically first), SIGKILLing the fresh flight seconds in — driver fixed (pre-launch snapshot of existing dirs).

## Resume pair — RED #3: the resume anchored on the checkpoint's LAUNCH dir, not the named --resume dir (order deep-research-t3a, appended 2026-08-17)

Third execution (post-hash-fix binary): kill flight `dr-1786980365` (SIGKILL after round 1, shape verified — checkpoint present, NO manifest/report). Tamper exercise: the driver COPIES the run dir to `tamper-copy/`, flips `charter_hash` → `deadbeef`, and resumes `--resume "$TAMPER_DIR"`. Measured red: exit 0 — the copy's resume COMPLETED A FULL FLIGHT (rounds 1-3, terminal manifest) and the ORIGINAL `dr-1786980365` became terminal — the resume anchored all state reads/writes on `config.run_dir` (the LAUNCH dir recorded in the checkpoint) and never read the tampered copy's checkpoint at all. The deadbeef tamper was never detected because the tamper copy was never read. Root cause: `run_dir` was treated as an identity field inside `config_mismatch` while simultaneously being the resume LOCATION. Fixed in the cmd layer: the named `--resume` dir IS the state home — `c.run_dir = resume_dir` before any state read/write, and `run_dir` removed from `config_mismatch` (it is a LOCATION, not identity — the charter, the identity, never included it). Regression tests: `resume_of_a_copy_anchors_at_the_named_dir` (a faithful copy resumes into the COPY, the ORIGINAL stays untouched; a deadbeef-tampered copy refuses "checkpoint tampered" and writes NO state into either dir). Also measured in this window: the kill driver's exclusion pattern (space-padded substring vs newline-separated snapshot) never matched, so the poll re-killed the prior dir — fixed with line-exact `grep -Fqx` exclusion.

## Resume pair — the GREEN: tamper refusal, mismatch refusal, honest continuation, ledger continuity (order deep-research-t3a, appended 2026-08-17)

Fourth execution (post-anchor-fix binary), kill flight `dr-1786981410` — the first clean kill (drivers + instrument reds all fixed). SIGKILL after round 1; killed shape verified at kill time (checkpoint `written_after_round = 1`, no manifest/verdict-set/report, stale `lock`). Then, against the SAME dir:

1. **Tamper** — `tamper-copy/` with `charter_hash` → `deadbeef`: `--resume` refused, exit 1, typed "checkpoint tampered" (tamper.console.log).
2. **Mismatch** — `--resume dr-1786981410 --max-rounds 5`: refused, exit 1, typed "--max-rounds 5 differs from the checkpoint's 3" (mismatch.console.log).
3. **Honest** — bare `--resume dr-1786981410` (no flags re-passed; the driver snapshots `budget-ledger.pre-resume.json` FIRST): console typed "continuing at round 2"; terminal `done-partial`, rounds [1, 2]; the checkpoint still reads `written_after_round = 1` and round-1 artifacts are intact (the killed shape is pinned post-resume too, strip 5).

Ledger continuity (recomputed by verify-demo11.sh from the pre-resume snapshot): every pre-kill entry appears exactly once in the final ledger, spent never decreases, remaining never increases, and per-meter `spent + remaining == allowance` holds (`web-search:mock` 12/12, `web-fetch:pages` 12/12) — identical budget arithmetic across the resume; a resume cannot double-spend the budget. Three reds measured and pinned before this green; 6/6 resume unit tests green.

## T3c — DECLARATION: the 122B judge swap, pre-registered BEFORE any re-judging (order deep-research-t3c, 2026-08-17)

Order deep-research-t3c is the verification arm: instrument validation (calibrate-judge.mjs with the 122B judge against the frozen calibration-bank.jsonl), the judge-swap re-judge of the frozen t2b DRB artifacts, the audit-forensics pass, and the written phase-2 recommendation. This entry is the §18.6 pre-registration — a judge swap is a judge change, and it is written before any re-judging. No judge is ever tuned in one direction; a re-measure is additive, never a rewrite.

### The judge swap, pre-registered (BEFORE)

- **Old instrument — the t2b measurement as-shipped, verbatim, never re-edited.** `Qwen3.6-35B-A3B-MTP-UD-Q6_K` at daemon :9741: demo9's `score-{local,hybrid,hybrid-delta}.json`, the README numbers (local pooled 0.8706 [0.7241, 1.0000], hybrid pooled 0.3571 [0.2564, 0.4554] vs reference 0.1737), and the four verdicts (P2 failed, P1 met, P4 passed, kill bar failed) all stand exactly as measured by this pin. The 35B numbers are old-instrument numbers and stay as published, unconditionally.
- **New instrument — the re-judge.** `Qwen3.5-122B-A10B-UD-Q5_K_XL` (the split, `-00001-of-00003`) as FACT judge via `drb-score.py`'s `FACT_MODEL` env override (the operator override always wins, line 52) AND as judge+critic for the chaos graded vocabulary (`svrn bench chaos-monkey score-answer`, judge-model + critic-model = the 122B stem at 127.0.0.1:9741). Same scorer, same bootstrap seed 4234932947, same frozen subset and SHA256SUMS. The chaos scorer's graded vocabulary — hallucination / grounded / caveated_ood / honest_abstention / answered_novalue — is applied per (fact, reference) pair.
- **Never mixed.** 35B numbers and 122B numbers appear only in tables that label both instruments by name. No old-instrument number is recomputed, back-filled, or replaced; the re-measure adds labeled rows.
- **The abstention dimension is scored separately, never collapsed.** The 139 unknown verdicts (134 local, 5 hybrid — the 35B could not tell) are re-classified into the graded vocabulary with honest_abstention reported as its own row; pooled fabrication stays on the pre-registered definition (unsupported/(supported+unsupported), unknown excluded). A re-measure that moves the rate must name what happened to the abstentions.
- **The calibration re-run and the DRB re-judge are DEFERRED-WAITING-WINDOW** — a named substitution (§18.3), never a silent drop. The 122B cannot load on the daemon today: memory probe 29Gi free vs ~70GB delta (daemon 18.7GB → ~88-92GB), mesh 1/6 peers online (distributed impossible; local-only fallback froze the box twice 2026-07-20 at rss 91255), swap 8/8. Seat-resolved 2026-08-17 (directive d98356d7): defer to a reserved window with ~90GB free, the July-13 pattern; config change, model swap, and daemon restart are seat-only actions. The marker the seat uses to wake this arm: **"the seat declares the 122B window"** — execution records for the deferred deliverables append to this entry when it fires. Until then: NO re-judging with the 35B (that substitutes the instrument the order exists to validate, defeating the point), NO config.toml edits, NO daemon restarts.

### The dr-verdict transition note (draft) — a re-judged number changes the bar's EVIDENCE, not its TEXT

The dr-verdict bar text (`demo/demo9/bars.md` and `quality/initiative-bars.toml` id `dr-verdict`, and its transitions) is frozen as pre-registered at t2b. Whatever the 122B re-measure says, it changes the bar's evidence only: demo9's measured-numbers table gains a re-measured row labeled by instrument (35B old / 122B new), and the P2 verdict section gains the re-measured CI with both instruments named. The bar's text — "ship iff P4 AND P2 AND P1"; P2 = hybrid-arm fabrication vs the 0.1737 reference — is not amended by any measurement, and no bar text is amended until the seat's transition decision. If the re-measure changes a verdict, the new measurement is reported beside the old one, which stays cited.

### Execution record — what landed (appended 2026-08-17)

- Audit-forensics enforcement-gap map + written phase-2 recommendation with cost: `research/deep-research/drb/T3C_AUDIT_FORENSICS.md` (new file beside drb/; runs/, the score files, and the subset untouched).
- No `.rs` files were moved by this order — the lint --full / test gate is reported as "none did", no gate run needed.
- **Seat steer (2026-08-17, added deliverable, ranked above the parked 122B half): the analytic gap inventory.** `research/deep-research/drb/T3C_GAP_ANALYSIS.md` — six ranked gaps derived ENTIRELY from existing artifacts (no judge calls, no flights, no daemon load): the admission decider's tie-lottery (4/4 hits tied at 0.03333, 117 below_cut, 42 rejects, 1 eps-quota admit; covered by heap items 6316d01c/4a140e88 — not duplicated), round-window rotation ("untraced: 68"; 127/161 = 79% union-window mismatch), DRB zero-pair asymmetry (3/10 local flights at fab=1.0; delta −0.5134 descriptive-only), abstention/decline instrument composition (139 unknowns; 7 decline-shaped), render pass-through (512/616 verbatim), two-arm lift at the ceiling (failed by letter 7×, direction flipped twice). Top five filed to the heap (objective deep-research, Evidence lines carry the artifact citations): 21370152 (4/M), d6862b5c (4/S), 4c5f1361 (5/M), 9f6ee143 (5/M), d8744bea (5/S) — all unvetted by design, the pull ritual's vetting is the review. Heap hygiene noted for vetting: 6316d01c's premise falsified by t2c's 0/10 (needs Approach update), 34bd60ae resolved at t2c (needs closure update).
- Claims released; session parked. The 122B deliverables — calibration re-run (a) and the DRB re-judge (c) — remain DEFERRED-WAITING-WINDOW with the condition named above; their execution records append here at the window.
