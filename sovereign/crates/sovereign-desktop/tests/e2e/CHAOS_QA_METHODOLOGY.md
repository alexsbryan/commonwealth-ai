# Chaos-QA: Measuring & Improving Desktop Answer Quality

**Purpose of this doc:** a self-contained handoff so a fresh session can pick up the
"run → measure → improve" loop that drives the Sovereign desktop app toward a
trustworthy answer-quality bar. Read this top-to-bottom before touching the loop.

Last updated: 2026-07-01. Author lineage: an autonomous quality loop; all code
changes are committed (see **Commit lineage**) except where noted **UNCOMMITTED**.

---

## 1. Mission & the quality bar

Drive the desktop app's answer quality up, measured by a **trustworthy, honest**
metric — never by teaching to the test. The quality definition (set by the product
owner, in their words) is:

> Approximate an **end user's** subjective judgment: *did they get a high-quality,
> ideally well-cited response?* The whole run should track to **"Can a user trust
> this application?"** — and **trust is kept by punishing confabulations.**

Concrete rules that fall out of that:
- **Do not shackle the model.** It MAY add correct general knowledge to connect
  facts or cover gaps. Correct GK is **good**, caveated or not.
- **Punish confabulation** — the trust-breaker: a **false** specific stated as fact,
  or an **invented quote / source / citation** (a `[Source: …]` that doesn't exist,
  a quote not in the evidence, "the text explicitly says X" when it doesn't).
- **Caveating and citation quality are quality *nudges*, tracked separately**
  (`caveated`, `well_cited`), **not** pass/fail gates.
- Honest declines ("the sources don't cover this") are **good**, not failures.

**Target:** ~85% honest composite. **Current trustworthy number: ~65%** on a
representative run (see §6). Earlier higher numbers (74–90%) were inflated by
measurement artifacts that have since been fixed — see §5.

---

## 2. Methodology — the non-negotiable discipline

This project follows SICP/SOLID-style rigor (see repo `CLAUDE.md`, `ARCH_PRINCIPLES.md`):

1. **Measure first, on a truthful metric.** A wrong metric sends you chasing
   phantoms. Half of this initiative was discovering the metric was wrong-low.
2. **No whack-a-mole. Instrument before fixing.** When a run flags a failure,
   *do not trust the label.* Reproduce it deterministically (temp-0 replay),
   turn on the gate trace, and **prove** the root cause three ways before changing
   code. Several "fabrications" turned out to be correctly-grounded answers the
   *harness* mis-measured.
3. **Generalized fixes only.** A fix must address a *class* of failure and be
   defensible from first principles — never a per-question patch.
4. **Prefer fixing the measurement over the app when the app is correct.**
   Tightening the gate to satisfy a broken oracle would break correct behavior.
5. **Small open-weight models** (SUT + judges are Qwen3.6-35B-A3B and below):
   keep every prompt **succinct and non-contradictory** — short *ordered*
   decision procedures (stop at first match). Long/parallel/conflicting rules
   degrade them. This applies to app prompts AND the offline judge prompt.
6. **The loop:** fix → deterministic **replay** of the prior run's questions to
   drive that run's score up → only spend a fresh 75-min run to test
   **generalization** once the replay confirms the fix. Don't burn a 75-min run
   to validate a fix a replay can validate.

---

## 3. The measurement apparatus

All under `sovereign/crates/sovereign-desktop/tests/e2e/scripts/`.

### 3.1 `chaos.mjs` — the run harness (the "brain / eyes / oracle")
- Spawns the SUT (`REPO_ROOT/target/debug/sovereign-desktop`; REPO_ROOT is the
  monorepo root `/commonwealth-ai`, **not** `sovereign/`) and drives it via the
  Tauri bridge on `:9745`. `--attach` wanders the **resident** corpora; `--spawn`
  spawns the desktop; attaches to the dev daemon on `:9741` for the 35B model,
  which plays **both** SUT and brain/judge.
- **Brain** (`brainPropose`, temp 1.0): the 35B invents the next "demanding user"
  move as JSON, given the command list + a running session-memory summary. So
  questions are genuinely LLM-generated, but **bounded by the fixed resident
  corpus set** (~28–34 corpora) — the same "landmark" facts (NARA file numbers,
  the SEP `¬Hn` formula, tokei `--files`, Enron people) recur across runs because
  the corpora and the adversarial exact-value strategy are constant. Variation is
  in phrasing / which row / corpus order, not the underlying fact space.
- **Conversation corpora** (`conversations-personal`, `-anthropic`,
  `conversation-history`) are **static pre-seeded fixtures** (timestamps predate
  the run) retrieved like any doc corpus; they contain seeded assistant turns but
  are NOT the app's own live output. Live conversation history is **endogenous**
  per run (scratch store, fresh conversationIds, "skips seeding"). So there is no
  cross-run second-order fabrication loop; the only in-run compounding path
  (retrieve an earlier turn as history) is neutralized by fixing fabrication at
  the root.
- **Live oracle** (`scoreAnswerAligned`): the SAME bench primitive
  (`assess_asserted_value`) the production gate uses. Needs the retrieved
  EVIDENCE, resolved to full text (see the capture bug in §5).
- **Journal:** `test-artifacts/chaos-journal.jsonl`, **wiped on start**. Copy it to
  a stamped file after each run. `SOVEREIGN_CHAOS_REPLAY=<bank>` replays a fixed
  question set deterministically (exits after one pass).
- **Gotcha:** `chaos.mjs` has NUL bytes — use `grep -a`.

### 3.2 `rejudge-length-blind.mjs` — the offline honest re-judge (**the metric**)
- Re-scores a journal's answers with the 35B on `:9741`, length-blind. This is the
  authoritative honest composite. Writes a per-step sidecar `{step, category,
  broken, well_cited, caveated, why}`.
- **Current rubric = trust-centric** (see §1): category ∈
  `good | honest_limitation | confabulation | incoherent`; `broken` = not in
  {good, honest_limitation}. `well_cited` + `caveated` are **tracked, non-scoring**.
- Rubric evolution (each a committed measurement fix): length-blind category rubric
  → `[unverified excerpt]` clause → truth-based (false_fact/false_attribution) →
  **pragmatic trust-centric** (current, **UNCOMMITTED** as of writing).
- Evidence window is 60 000 chars (see §5 — must fit ALL retrieved chunks).

### 3.3 `summarize-rejudge.py` — aggregate a sidecar
`python3 summarize-rejudge.py <sidecar.rejudge.jsonl> <journal.jsonl>` →
composite %, per-category counts, broke detail (with Q + answer head), and a
unique-question dedup (the brain re-asks; dedup so repeats don't skew).

### 3.4 `launch-representative-run.py` — detached 75-min run
Double-fork + `os.setsid` so the harness reaper can't SIGKILL it mid-flight
(a plain `run_in_background` waiter **gets reaped** on multi-hour runs). Writes a
stamped journal + a `.DONE` sentinel. Edit the STAMP and minutes per run.
**Monitor** completion by polling the `.DONE` file on a ScheduleWakeup, not a tight loop.

`launch-citefix-replay.py` / `launch-truncfix-replay.py` are the REPLAY variants
of the same pattern: temp-0, `SOVEREIGN_CHAOS_REPLAY=<bank>`, gate-trace +
`synth.citation` RUST_LOG on, and they also copy the app log to
`<stamp>.app.log` (essential — the per-turn mechanism traces live there). Build
a bank with `build-replay-bank.mjs <journal> <bank>`; edit BANK/STAMP per run.

### 3.5 Instrumentation — the gate trace (**critical, easy to miss**)
The grounding gate logs under tracing target **`grounding_gate`** (a custom
string). `RUST_LOG=sovereign_core=debug` does **NOT** match it. To SEE the gate:
```
RUST_LOG="sovereign_desktop=info,sovereign_core=info,grounding_gate=debug,sovereign_inference=info"
SOVEREIGN_AGENTIC_KQ_DEBUG=1        # routes dbg() via a captured target
SOVEREIGN_SYNTH_TEMP=0             # determinism for repro
```
`chaos.mjs` honors a pre-set `RUST_LOG` (`process.env.RUST_LOG ?? default`). The
app log lands in `test-artifacts/chaos-app.log`. Grep `citation:` /
`longform ` / `specifics_scan` lines.

---

## 4. The app's grounding gate (what we're measuring)

`sovereign-core/src/runtime/grounding/` — `gate_answer` (mod.rs:272) routes by
length at a ~1800-char pivot (`SOVEREIGN_LONGFORM_CHARS`):
- **Short path** (`gate_answer`): citation-grounding (copy a verbatim supporting
  quote → `quote_present_in_chunks` + `answer_supported_by_quote`) → single-claim
  verify → retry → abstain. Releases `ANSWER\n\nGrounded in the source: "QUOTE"`.
- **Long path** (`gate_longform`, mod.rs:872): per-claim audit (`extract_claim_list`
  budgeted by `claim_budget`) + **holistic `scan_unsupported_specifics`**
  (committed `b7e51bf6`, default ON) → rewrite / annotate.
- `gate_on = gate_surface.enabled() && documents_found > 0`. **The gate grounds on
  `gate_evidence_chunks(&chunks)` — the ENTIRE retrieved set (uncapped, minus
  raptor).** This fact is central to the §5 capture bug.
- Env flags live in `grounding/config.rs::grounding_gate_flags()`.
- `quote_verification.rs` rewrites spans it can't verbatim-confirm to
  `[unverified excerpt: X]` — an **honest** glassbox label (judge X's content,
  not the wrapper).

---

## 5. Pivotal findings — the metric was wrong-low (two capture artifacts)

The dominant "fabrication" residual was **measurement error**, proven by
instrumentation, not app fabrication. Two capture bugs, both fixed:

1. **Per-chunk truncation** (`8edd6f55`): `chaos.mjs` truncated each chunk to 1500
   chars while the gate grounds on full content. A grounded specific past char 1500
   read as fabrication. Fix: capture 12000/chunk.
2. **Chunk-SET top-12 cap** (`4100ca31`, the big one): `resolveChunkTexts` sliced
   the retrieved chunks to the **top 12** before the oracle, but the gate grounds
   on **all** retrieved chunks (up to 39). An answer whose supporting quote lived
   in a chunk ranked 13th+ was judged against evidence that omitted its own
   grounding chunk → scored fabrication. **13 of 15** gen75 "fabrications" had
   `retrieved > 12`. Proven 3 ways: gate trace (`present=true → GROUNDED`), corpus
   grep (`Foo(42` verbatim in the tokei Lance index), and the same question passing
   at rank ≤12 / failing at rank 13+. Fix: resolve all chunks (`slice(0,48)`),
   `evidence.text` cap 120k→300k, re-judge window 12k→60k. **Validation:** temp-0
   replay of previously-broke turns → **8/9 flip to good**; `resolved == retrieved`.

Also fixed as measurement calibration: the succinct rubric (`8edd6f55`) and the
`[unverified excerpt]` honest-label clause (`bf56bac9`).

**App-side fix committed** (`b83ec57e`): exact-value + GK fidelity in the short
gate path — (1) a numeric answer token must match a COMPLETE digit-run in the quote
(kills `289494` grounding against `28949423`); (2) strip the "from general
knowledge" caveat unconditionally before verifying (kills confident GK
fabrication). Gated `SOVEREIGN_EXACTVAL_FIX` (default ON). Validated on replay:
NARA ×3 broke → ×3 good. The shelved iter3 short-specifics guard is
**default-OFF** (`SOVEREIGN_SHORT_SPECIFICS_SCAN=1` to enable).

**Do NOT tighten the gate to satisfy the oracle** — the gate grounds correctly;
these were measurement bugs.

## 5b. App-side fixes (2026-07-01 session two — both validated by paired replay)

1. **Citation fidelity — snap + ID-token veto** (`bc0dd31f`,
   `citation_attribution.rs`): `title_is_supported` → ordered `judge_title`
   (exact label → bounded verbatim phrase → **snap** a unique near-miss of one
   real label back to that label → **ID-token veto** for ID-shaped tokens with
   no complete-token match → word floor). Root cause: an opaque id splits into
   prefix+hash; the intact prefix alone scores exactly 0.5 = the keep floor.
   Snap calibration has margin on both sides (garbles 0.80–0.95 vs fabrications
   ≤0.53; floor 0.75, uniqueness margin 0.10). Watch `synth.citation` telemetry
   (`snapped=[…]`) to see it fire.
2. **Mid-token completion** (`b1f09a19`, `citation.rs`): the MTP primary emits a
   spontaneous EOS mid-token under long-context copy load (probed: finish=stop
   at 99/256 tokens) — the short path released `…SYSTEM_PROM` / `∧ ¬`-dangling
   formulas. `extend_mid_token_copy` completes the tail deterministically from
   the verified quote/chunks (unanimous continuation required; ≤24-char runs;
   non-prefix garbles still fall to reject→retry). Detect truncation by
   CONTENT, never finish_reason — that trap is now closed on the extraction
   path too.
3. **Longform output quality** (`124eaf13`, judge.rs/mod.rs/streaming.rs): the
   gate's own honesty surfaces stopped indicting the answer. Scan findings
   reduce to verbatim answer spans (`normalize_scan_item`); `[Source:]` markers
   are outside the scan's jurisdiction (pre-validated by the pre-gate snap);
   `[unverified excerpt:]` wrappers are stripped before any judge sees them
   (they biased the audit against SUPPORTED content — Samuelson-1954, verbatim
   at offset 2410, flagged only when wrapped); the rewrite may not mint new
   claims-about-the-sources; and `verification_note` renders plain, deduped,
   capped items — **unquoted by design**: the post-synthesis quote guardrail
   demotes curly-quoted non-verbatim spans, and the note's cosmetic quotes made
   the app's two honesty mechanisms fight each other (the probed
   self-contradicting footer). Watch `verification note rendered` in the trace.
4. **Snap hardening** (`13e72611`, citation_attribution.rs): unconditional
   uniqueness margin (label FAMILIES defeat the old floor-escape — the
   "Articles II–XI"→"Pets" false snap), composite hyphen-digit veto (date
   garbles "2026-10-10"), parenthetical-qualifier snap ("Wikipedia
   (contested)"→label), bounded bracket scan (an unclosed `[Source:` swallowed
   ~570 chars into the note).
5. **Measurement** (`50591afa`): journal `evidence.labels` (chunk titles +
   corpus ids); label-aware re-judge rubric (+"Verification note is honest"
   clause); replay mode isolates each bank question in a fresh conversation
   (cross-question contamination proven). **Never co-schedule the cargo test
   suite with a SHORT replay** — the compile burst starves the SUT (one round
   voided by 60–130s hangs).

---

## 6. Current state (2026-07-01, post citation-fidelity + mid-token fixes)

- **Trustworthy re-baseline** (`rebaseline-2026-07-01`, fixed harness,
  representative, 41 answered): **~65% trust-centric composite** (12 confabulation
  + 2 incoherent of 41; **75% on the unique-question view** — the comparable
  number for deduped replays). Tracked signals among good answers: **well_cited
  70%, caveated 18%**.
- Old-rubric split on the same run: **focused fact-lookup 77% / open-ended
  "most important thing in X" 50%** — the open-ended synthesis path is the weak spot.
- The failures were **genuine trust-breakers**, verified against full evidence:
  1. **Fake / corrupted source citations**: step 21 cited *four* `[Source:
     watched-…]` IDs, all nonexistent corruptions of the one real corpus ID
     `watched-959ee8a8f330`. LLMs can't reliably copy opaque hash IDs. Also hit
     labels with dates ("2025-06-10" for the real "2026-06-10" — step 171's
     "invented date" was a garbled LABEL) and section titles (Federalist
     stutter-titles, step 175).
  2. **Synthesis padding** (~7): invented in-prose specifics on open-ended prompts.
  3. **Truncation** (2): answers cut off mid-value.

**Both №1 and №3 are FIXED and validated** (see §5b). Paired temp-0 full-bank
replay (`citefix-replay-2026-07-01`, pre-truncation-fix binary): 5 unique
confabulations flip to good/honest; 13/13 correct snaps, 0 false snaps, 0
over-strips on 20 still-good questions; unique composite 75% with the remaining
broke = the truncation pair (since fixed — `truncfix-replay-2026-07-01` shows
both release complete answers) + in-prose padding + two known intermittent
classes (leak-loop repetition; SEP-lighthouse rewrite misattribution).

### LOOP CONVERGENCE (2026-07-02): calibrated 90% raw / 91% unique — TARGET MET

| Run | Measured (raw rubric) | Calibrated (`--verified`) | Fixes landed before it |
|---|---|---|---|
| rebaseline | 65% | — | — |
| gen75 | 81% | — | citation snap/veto, mid-token completion, output-quality stack |
| gen75b | 83% | — | title provenance (root cause), alignment, structural strip, case-snap |
| gen75c | 83% | **90% / 91%** | unclosed-recovery, attribution veto (+ post-hoc: space-respace, handle strip) |

Three independent measurements converge on gen75c ≈ 90%: the calibrated
verified-judge, the hand receipt-audit of every broke verdict, and the
deterministic outcome audit (0 invented sources, 0 truncations, 0 leaks,
0 unclosed citations across the journal). The measured raw number plateaus at
83% because the JUDGE's specificity (38% on receipt-verified good answers —
see the calibration gate below) became the binding constraint, not the app.

**Trust-lens read (the product bar):** 0 severe betrayals (invented source /
garbled value-as-fact) in gen75b+c audits; fail-safe abstentions verified in
traces (the gate refusing a twice-garbled phone number); residual ≈ one
unverifiable interpretive specific per 15–20 answers, concentrated in
degenerate open-ended prompts — 35B-model-bound (padding drift at temp 0.7,
false-premise acceptance, literary extraction). Next levers if pushed further:
prompt-side premise-checking, or a stronger draft model — not more gate
machinery.

### PUSH-PAST-90 LOOPS (gen75d/e/f, 2026-07-02) — CEILING REACHED ≈ 90 ± 3

| Run | Raw | Verified | Note |
|---|---|---|---|
| gen75d | 88/89 | 90/91 | identifier veto, fragment guard, premise rule |
| gen75e | 77/78 | 82/81 | harder+bigger draw (78 answered); exposed the under-pivot ghost band |
| gen75f | **89/89** | **89/91** | short-path sweep; raw–verified gap = ZERO |

Fixes landed in these loops: identifier-attribution veto (+ sentence-level
sweep in BOTH gate paths — the ghost family had been hiding in the
1,500–1,800-char band UNDER the longform pivot), fragment guard
(abstained_fragment), premise-check synthesis rule, space-respace, phantom
[ev-…]/[passage N] strip. Outcome audits: gen75f had ZERO known-ghosts, zero
unclosed citations, zero truncation tails; 41/42 citations verified.

**Ceiling evidence:** raw converged to verified (the app no longer produces
judge-confusing shapes); every deterministic class is dead across ≥2
consecutive runs; the verified-broke set is heterogeneous SINGLES:
interpretive picks on degenerate open-ended prompts, real-world-true GK
quotes the judge counts against evidence (Noam Cohen ×3 across runs — a
judge-philosophy boundary, not fabrication), literary attribution nuance,
and ~1/run rare leak variants. Remaining systematic item (queued):
empty-retrieval ungated path (retrieved=0 → gate off → speculative tail;
fix = scoped-zero-hit honest decline template). Past this: stronger draft
model, or accept ≈90 ± 3 (mix-dependent) as the 35B tier's resting state.

### The judge-calibration gate (anti-gaming — read before touching the rubric)

`tests/e2e/calibration-bank.jsonl` = 18 receipt-verified (question, answer,
evidence, labels, gold) cases; `calibrate-judge.mjs` scores any rubric against
it (sensitivity floor 0.85 / specificity floor 0.8; exit 1 on failure). **No
rubric or judge change may score runs without passing this gate.** Measured
history: raw rubric 100%/38% (frozen v0 identical — rubric edits to date were
calibration, not gaming); prompt-language "fixes" FAILED the gate (verify-first
framing dropped sensitivity to 70%; a fuzzy word-overlap overturn cleared a
proven date garble). What passed: the deterministic verification layer
(decline-shape override + all-must-verify disputed-string greps) → 100%/75%,
with the 2 residual FPs documented as contested boundary cases.
`rejudge-length-blind.mjs --verified` applies it; report runs BOTH ways.

### GEN75 GENERALIZATION RESULT (`gen75-2026-07-02`, representative 75-min, ALL fixes)

**Raw 81% (65 judged, 12 broke: 8 confab + 4 incoherent) / unique 83% (54
unique, 9 broke). well_cited 71%, caveated 11%.** Up from 65%/75% at the
rebaseline; landed inside the pre-registered prediction (raw 78–84). The fixed
classes held at generalization on fresh temp-0.7 questions: **96% of released
citations exact-match a real source label; 0 invented/garbled sources; 0
truncations; 0 note self-contradictions** (offline audit of all 67 answered
turns — the representative launcher runs trace-light, so audit the journal, not
the app log). Throughput 67 answered turns vs 41 at rebaseline (fewer wasted
rewrite cycles).

### Post-gen75 lever (landed in `6d6d25ee`): residual №1 + №2 FIXED

1. **Value-misattribution root cause = MISLABELED EVIDENCE, not the model.**
   Probed four layers down: the gate's alignment input showed 3 distinct
   titles across 14 chunks vs ~10 real row titles. The dominant-source
   cohesion expansion (`retrieval.rs`) built neighbour rows by cloning the
   anchor hit — every positional neighbour INHERITED THE ANCHOR'S TITLE.
   Sound for one-title books; false for row-per-document corpora (the UAP
   index: one source_doc, many case files). The synthesis prompt therefore
   showed the Stevens Point row under the SAT header — the model cited what
   it was SHOWN. Fix: neighbours keep their own title/url. Probe: distinct
   titles 3→13; the same temp-0 question now releases the coherent pair
   "302569447 [Source: Stevens Point, Wisconsin ()]". Defense in depth:
   `align_citation_values` (citation_attribution.rs, wired post-gate) is the
   deterministic backstop — ID-shaped values in a citing segment must live in
   the cited chunk; unique-holder mismatches re-point the citation, ambiguous
   ones strip it. `EvidenceContext.chunk_labels` (per-chunk, raptor-aligned)
   carries the mapping; `stage="align"`/`align_input` traces in
   `synth.citation`.
2. **Tool-call leak: structural stripping, no name lists** (`presenter.rs`
   `strip_bare_tool_call_lines`): (a) anywhere — whole unfenced line of
   `identifier(query=/search=/…)` kwarg-call syntax; (b) terminal — last
   content line is `identifier(…)` (ANY name) announced by a FIRST-PERSON
   intent line ("Let me …:"): the model handed off to a tool that doesn't
   exist and stopped. Imperative instructional endings ("Call it like
   this:\n\nfoo(42)") survive. Both observed reflex shapes validated leak-free
   on replay; tool-calling paths unaffected (envelope parsing reads raw
   completions; RecipeAuthor exempt; structured calls never travel as prose).

**Residual broke (unique ≈7 at gen75; ~4 after this lever), the honest gap to 85%:**
1. **Value-misattribution to a real source** (NARA ×3-reask + maple ×2): the
   answer cites a REAL retrieved label but the value/claim belongs to a
   DIFFERENT retrieved file ("28940827" from another Blue Book file; a window
   rule cited to the guest-policy decision). The known gate blind spot, now the
   TOP lever: claim↔source alignment, not source existence.
2. **Tool-call leak on an unindexed folder corpus** (×3 same Q):
   `knowledge_lookup(query=…)` syntax released as the answer.
3. **Formula case-fidelity** (`¬HN` for `¬Hn`): the substring check is
   case-insensitive by design; a case-strict rule for math/code tokens is the
   candidate fix.
4. One over-abstention (declined with the value present); one `[BLANK]`
   redaction-artifact citation (marginal).

---

## 7. Open threads / next steps (ranked by trust impact × fixability)

0. **DONE (see §5b items 3–5):** the longform output-quality lever below was
   instrumented and fixed across four replay rounds on 2026-07-01 (all of 1a–1d
   plus three defects those rounds newly exposed: the wrapper-vs-audit bias,
   the note-vs-quote-guardrail fight, and snap-family false snaps). What
   remains of the padding class after those fixes: open-ended INTERPRETIVE
   claims on degenerate "most important thing" prompts and hard literary
   extraction (Verloc/Federalist) — 35B-model-capability territory, plus
   chimera labels (real date + wrong name; one observation) and per-claim
   judge precision on supported claims (Mill vp=0.634 within caps). Next
   ranked levers below, pending the gen75 generalization run.

1. **Longform rewrite/annotate output quality (recommended next).** Instrumented
   2026-07-01 (citefix-replay app log): audit RECALL is decent (the false "James
   Joyce wrote *To the Lighthouse*" failed per-claim at vp=0.981 AND was
   scan-flagged) — the trust-breakers ship at the OUTPUT stage. Four concrete,
   general defects, each observed live:
   a. `scan_unsupported_specifics` returns judge CHATTER, not verbatim answer
      spans ("The answer cites …", "— The evid…"), and that chatter flows
      unsanitized into the user-visible verification note (step 20's "is a
      fabricated specific" self-indictment). Fix: verbatim-only output contract
      + sanitize before the note.
   b. The REWRITE can introduce new claims-about-the-text (step 29: replaced the
      Joyce misattribution with "the text cites Woolf's work" — correct GK, but
      the text names no author). Constrain the rewrite: prune/hedge only, never
      new attributions to the sources.
   c. `rewrite_annotated` keeps confidently-asserted failed claims in the BODY
      with only a footnote — the assertion still reads as fact.
   d. Ordering: the scan sees the PRE-snap draft, so garbled `[Source:]` labels
      get flagged and burn rewrite cycles before the post-gate snap cleans them.
      Consider snapping citations before the audit.
2. **In-gate evidence caps (same artifact class as §5, inside the app).**
   `scan_unsupported_specifics` truncates each evidence chunk to 1500 chars
   (judge.rs:397); the per-claim check caps at top-12 chunks × 2400 chars
   (judge.rs:251/255). Mirror-images of the two PROVEN capture artifacts — a
   grounded specific past the cap / rank 13+ cannot be rescued and reads as
   fabricated, feeding rewrite churn. On the replay's observed failures these
   bit less than 1a–1c, so quantify first (deterministically grep each failed
   claim against the FULL journal evidence) before changing the caps.
3. **Invented PROSE source labels.** Fixing hash garbles exposed a second-order
   mode (replay step 20): `[Source: Boy who never landed]` — a descriptive
   invented label whose common words pass the word floor. One occurrence so far;
   candidate fix is a stricter floor when real labels exist and neither
   exact/snap/phrase matched, but get more observations first.
4. **Leak-loop repetition** (replay step 28, known intermittent class): quote
   text duplicated/concatenated inside `[unverified excerpt: …]`. Root-cause via
   quote_verification path when it recurs.

---

## 8. Runbook

**Preconditions:** dev daemon up on `:9741` with the 35B
(`Qwen3.6-35B-A3B-MTP-UD-Q6_K_XL`); SUT built at `target/debug/sovereign-desktop`
(rebuild with `cargo build -p sovereign-desktop --bin sovereign-desktop` after app
changes — harness/JS changes need no rebuild).

**Representative run → honest number:**
```
# edit STAMP/minutes in launch-representative-run.py, then:
python3 tests/e2e/scripts/launch-representative-run.py          # detached, writes <stamp>.DONE
# wait for <stamp>.DONE (poll on a ScheduleWakeup), then:
node tests/e2e/scripts/rejudge-length-blind.mjs <stamp>.jsonl <stamp>.rejudge.jsonl
python3 tests/e2e/scripts/summarize-rejudge.py <stamp>.rejudge.jsonl <stamp>.jsonl
```

**Deterministic replay (validate a fix on a prior run's questions):**
build a bank of `{cmd:"send_message_stream", scopedCorpus, args}` lines from a
journal, then run `chaos.mjs --attach --spawn` with
`SOVEREIGN_CHAOS_REPLAY=<bank> SOVEREIGN_SYNTH_TEMP=0` (+ the gate-trace envs from
§3.5 to root-cause). Re-judge the resulting journal.

**Detached long runs:** always launch via the double-fork pattern in
`launch-representative-run.py` (PPID 1, reaper-immune). Plain `run_in_background`
waiters get reaped on multi-hour work.

**DoD for app changes:** `scripts/sovereign-lint.sh --human` + `scripts/sovereign-test.sh --human`
(the watcher may be `not_configured` — fall back to these full-workspace scripts,
never a narrowed `cargo -p`).

---

## 9. Commit lineage (this initiative)

- `50591afa` fix(chaos-measurement): journal source labels, label-aware rubric, replay conversation isolation
- `13e72611` fix(grounding): citation snap hardening — margin always, composite date veto, qualifier snap, bounded brackets
- `124eaf13` fix(grounding): longform output quality — the gate's own honesty surfaces stop indicting the answer
- `b1f09a19` fix(grounding): complete mid-token generation stops from the verified source
- `bc0dd31f` fix(grounding): snap garbled [Source:] labels to the real source + ID-token veto
- `8ac9773d` docs(chaos-qa): methodology + handoff, persist tooling, **trust-centric rubric**
  (the rubric IS committed here — an earlier revision of this doc said otherwise)
- `4100ca31` fix(chaos-measurement): judge against the gate's FULL evidence set (all chunks) — the top-12 cap fix
- `bf56bac9` fix(chaos-measurement): recognize the honest [unverified excerpt] label
- `b83ec57e` fix(grounding): exact-value + GK-fabrication fidelity in the short gate path
- `8edd6f55` fix(chaos-measurement): judge against the gate's FULL evidence + calibrated succinct rubric
- `b7e51bf6` feat(grounding): holistic specifics-scan closes the gate's fabrication blind spot
- **Untracked (deliberate):** `test-scan-short.mjs` — shelved iter3 tool.

Persistent memory index: `~/.claude/.../memory/MEMORY.md`; deep notes in
`project_chaos_evidence_capture_artifact_2026_07_01.md` and its `[[links]]`.
