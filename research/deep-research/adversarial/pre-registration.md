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
