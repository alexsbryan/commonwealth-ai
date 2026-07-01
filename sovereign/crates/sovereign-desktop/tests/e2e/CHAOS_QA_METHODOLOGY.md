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

---

## 6. Current state (2026-07-01)

- **Trustworthy re-baseline** (`rebaseline-2026-07-01`, fixed harness,
  representative, 41 answered): **~65% trust-centric composite** (12 confabulation
  + 2 incoherent of 41). Tracked signals among good answers: **well_cited 70%,
  caveated 18%**.
- Old-rubric split on the same run: **focused fact-lookup 77% / open-ended
  "most important thing in X" 50%** — the open-ended synthesis path is the weak spot.
- The 65% failures are **genuine trust-breakers**, verified against full evidence:
  1. **Fake / corrupted source citations** (clearest): step 21 cited *four*
     `[Source: watched-…]` IDs, all nonexistent corruptions of the one real corpus
     ID `watched-959ee8a8f330`. LLMs can't reliably copy opaque hash IDs.
  2. **Synthesis padding** (~7): invented dates (`2025-06-10`), a phantom
     "Three Expedients" list, etc. on open-ended prompts.
  3. **Truncation** (2): answers cut off mid-value.
  - Judge noise: ~1 false positive observed (step 166 cited a real source but was
    flagged) → treat 65% as ±a couple points.

---

## 7. Open threads / next steps (ranked by trust impact × fixability)

1. **Citation fidelity — fake/corrupted source IDs (recommended next).** The app
   asks the model to reproduce opaque hash IDs and it garbles them into nonexistent
   `[Source: …]`. Generalizable fix: post-process citations — validate each cited
   source against the real retrieved source labels; snap fuzzy matches to the true
   ID, strip citations that match nothing. A citation to a nonexistent source is
   the cardinal trust-breaker. **Instrument first** (gate trace + which path emits
   the citation) before coding.
2. **Synthesis padding** on open-ended prompts. Larger count, subtler. The 18%
   caveat rate is the lever: nudge the model to **label** GK ("from general
   knowledge, not your sources") rather than pass it off as sourced. `gate_longform`
   already has `scan_unsupported_specifics` — instrument *why it misses* these
   (short vs long path? budget? the "most important thing in <corpus-id>" prompts
   are partly degenerate chaos questions — real users ask more focused things).
3. **Truncation**: answers cut off mid-value (see prior truncation work in memory —
   FastFocused 600-cap, MTP-always-Stop; detect by CONTENT not finish_reason).

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

- `4100ca31` fix(chaos-measurement): judge against the gate's FULL evidence set (all chunks) — the top-12 cap fix
- `bf56bac9` fix(chaos-measurement): recognize the honest [unverified excerpt] label
- `b83ec57e` fix(grounding): exact-value + GK-fabrication fidelity in the short gate path
- `8edd6f55` fix(chaos-measurement): judge against the gate's FULL evidence + calibrated succinct rubric
- `b7e51bf6` feat(grounding): holistic specifics-scan closes the gate's fabrication blind spot
- **UNCOMMITTED:** `rejudge-length-blind.mjs` — the pragmatic **trust-centric** rubric
  (this session's current metric). `test-scan-short.mjs` — untracked shelved iter3 tool.

Persistent memory index: `~/.claude/.../memory/MEMORY.md`; deep notes in
`project_chaos_evidence_capture_artifact_2026_07_01.md` and its `[[links]]`.
