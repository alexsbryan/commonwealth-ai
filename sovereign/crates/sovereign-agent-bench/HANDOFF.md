# `sovereign-agent-bench` — session handoff

Continuation doc for the agent-coding battery. Pairs with
`/Users/user/dev/commonwealth-ai/HANDOFF.md` (the predecessor
OICP-runner diary) and the plans at
`~/.claude/plans/i-want-to-pickup-sorted-eagle.md` (original) +
`~/.claude/plans/autonomous-loop-tick-tingly-clock.md` (PR 1
canonical-tools crate) + `~/.claude/plans/role-layer-multilang.md`
(PR 2 role layer).

---

## Methodology — convergence as correctness criterion

(Carried forward verbatim across PRs because it's load-bearing.)

This work is architectural, not feature work. Per ARCH §0.4 ("don't
whack moles") and the user's stated convergence test, every change
in the agent-bench / canonical-tools layer must satisfy:

1. **Class identification.** Every primitive, role, detector, or
   adapter rule corresponds to a NAMED CATEGORY of model behavior
   — a meta-skill (verify-before-iterate, inspect-before-mutate,
   terminate-cleanly) or a NAMED failure class (write-thrash,
   parse-fail-envelope, attention-diffusion). Not a fix for one
   observed trial.

2. **Analytical closure.** Argue *why* the change closes a class of
   failures we may not yet have observed, with reference to the
   shape of the contract. "Splitting the Implementer from the
   Evaluator role" closes the entire write-thrash class because the
   tool subset structurally excludes the iterate-without-verify
   shape — not just the 2.1 instance.

3. **Convergent set, not à la carte.** Adding a primitive, role,
   or detector variant is a commitment. Resist adding more every
   time a problem surfaces a need; ask whether the surface can be
   composed from the existing N. "Make the essential primitives"
   (user 2026-05-21) — primitives are essential when removing
   them would break a class, not just an instance.

4. **Pin invariants with tests** (ARCH §7.2, §12.3). Cross-adapter
   equivalence (`pi_and_native_expose_the_same_canonical_set`),
   role tool-subset disjointness, role transition rules, dossier
   render caps — every contract is pinned by a test that fails
   when a future PR softens it. Convergence enforced in code, not
   in a wiki.

5. **Glassbox** (ARCH §0.1, §9). Every adapter translation, every
   primitive execution, every role transition emits a
   `tracing::info!` or `debug!` event. Operator reading
   `RUST_LOG=sovereign_agent_tools=debug,sovereign_agent_bench
   ::runners::native=debug` can reconstruct the whole loop.

6. **Don't coach to test** (user, 2026-05-21). Discipline that's
   universal engineering practice (verify-before-iterate,
   inspect-before-mutate, terminate-cleanly) goes into the tool
   contract or role structure. Discipline that's algorithm-specific
   (use GF(2) for Lights Out, use a hashmap of sorted-keys for
   anagrams) stays in the problem prompt at most. The bench is a
   measurement instrument; coaching distorts the measurement.

7. **Measure honestly, then iterate** (user, 2026-05-21). If the
   architecture lands an honest measurement that the proposed
   meta-skill DIDN'T close the gap (e.g. PR 1's tool-naming
   hypothesis didn't move 2.1/3.2), that's a *result*, not a
   failure. The architecture earns its weight as the measurement
   instrument, and the next iteration targets a sharper gap.

The roles + primitives + transitions in PR 2 are framed by these
seven. When in doubt about adding a new role / primitive / detector,
walk down the list and ask "does this change satisfy criterion N?".

---

## 2026-07-06 — run-measure-improve loop, iteration 1 (search runner, full battery)

Mission (operator): drive the battery to consistent perfect scores by
improving the HARNESS around the open-weight model — "the model IS
capable of the presented tasks but often only with the right context
and system around it." Methodology imported from the chaos-QA loop
(`sovereign/crates/sovereign-desktop/tests/e2e/CHAOS_QA_METHODOLOGY.md`):
instrument first, receipts before diagnosis, class fixes only,
paired replay validation.

**Instrument (committed `bda56ec7`):** per-candidate receipts. Trial
trajectory labels now carry the failure class (`err:parse` /
`err:apply` / `err:backend` / `err:snapshot`), and `RoundSummary`
gains `details[]` (full error capped 600ch, body_chars, 200-char
body_tail) persisted into `requests.jsonl`. Before this, a stalled
trial was 12 bare `err`s — undiagnosable from artifacts.

**Classes found + fixed this iteration (each generalizes; per the
operator: never target the problem/language under test brittle-y):**

1. **NoBaseline scaffolds (fixed `6f3619b5`).** 1.1/1.2/1.3 shipped
   zero scaffold smoke tests → the TDD Machine died in 227ms
   ("test_command produced no test results") — a structural 0
   indistinguishable from failure (same invisible-zero family as the
   2026-06-08 `[lints]` bug). Authored 3 smoke tests each, encoding
   ONLY the prompts' own worked examples, per the 8/11-problem
   convention. All three problems went 0 → 9/9 (3/3 trials each).
   DEEPER generalization queued: solver-side fallback (no tests →
   `GenerateOneFailing` Red polarity) so future problems never need
   hand-authored smoke tests.

2. **`replace_function` was Python-only (fixed `2e4be993`).**
   `find_function_bounds` knew `def`/`class` + indent walking. Every
   `rewrite <fn>` against a brace language died with "no function or
   class named X found" — with the stub in plain sight. Receipts:
   9/12 candidates on 2.2 and 10/12 on 3.2-lights-out (Rust) died to
   exactly this; the model's PREFERRED move (function-granular
   rewrite — the right-sized emission) was structurally rejected,
   forcing it into whole-file writes, which is where its Rust
   fluency actually breaks. Fix: layered finder — indent walker
   (unchanged, Python), syn span lookup (Rust — existing dependency,
   parser-grade, attrs included), textual keyword-introduced
   brace-function family `fn`/`func`/`function` (Go/TS + fallback).
   7 pinning tests.

3. **Reasoning-leak-into-code (OPEN — next lever).** Controlled
   probe (solver's exact round-0 prompt, 4 calls): 0/4 Rust
   whole-file emissions compile; thinking surfaces INSIDE the source
   block ("// Wait, I made an error in the Gaussian elimination
   swap…"). NOT truncation (1821–2214 toks < 2500 cap, fences
   closed). The trial system prompt forbids commentary outside the
   fenced blocks — the model has no sanctioned thinking channel (the
   same flaw CI_GATE_HANDOFF flagged for pi). Candidate fixes:
   sanctioned pre-block thinking (prose outside fences is already
   structurally ignored by the parser), and/or a pointed
   syntax-repair turn on syn rejection (small models fix pointed
   errors well). ALSO: `prompt.md`'s "How to deliver" tail
   (write-tool + "do NOT paste the solution into chat") contradicts
   the search runner's paste-code contract — delivery instructions
   belong to the runner, not the problem statement.

**A-arm baseline (pre-fix binary, `target/agent-bench/baseline-2026-07-06/`):**
1.1/1.2/1.3/2.1 = 9/9 ×3 each (post smoke-test fix); 2.2 = 0/8/6
(variance = whether the model recovered from the broken finder by
switching shapes); 3.2-rust = 0 ×3 (10/12 finder + 2/12 backend).
**Everything from 3.2-python onward is VOID — the daemon was
jetsam-killed mid-run** (all candidates `err:backend` transport;
log cuts mid-inference-setup, no error line; auto-restarted ~08:17).
3.3's "1/9 completed" ×3 is void too. Known §E class.

**B-arm (fixed binary, fresh daemon, `target/agent-bench/barm-2026-07-06/`):**
full battery ×3 trials, running as of this entry — the paired replay
for fixes 1+2 and the first honest full-battery measurement.

**Arms B–E and iterations 2–6 (same day).** Full trial log in
`target/agent-bench/{baseline,barm,darm,earm}-2026-07-06/`.

B-arm (finder+smoke fixes): grand 69/99. Tier-1 ×4 + 3.2-py all 9/9;
2.2 = 8/9 ×3 (judge anchor); 3.2-rust 9/0/0 (emission variance);
3.3 2/9 ×3 (goal outside fitness signal);
4.1 6/9 ×3 (judge parser bug); 4.2 8/6/9; 5.1 0 ×3 (single-file
tunnel vision).

Iterations 2–6, all committed, each receipt-anchored + family-level:
- d961369a header inference (~ labels) + delivery-section strip
- 3209399b pointed syntax-repair turn (+r labels)
- a6e6a15a 3.3 structural red test (goal INTO the fitness signal)
- 23a7b937 multi-file addressing (render all sources; cross-file
  rewrite resolution; write_file{path})
- 5e99ea82 judge anchor accepts numeric strings ("3") — 4.1's whole
  3-point gap
- 5e97447f 600s queue-aware backend timeout (K parallel candidates
  serialize on one local slot; 180s killed the tail = fake
  err:backend ~25% tax) + minimal-change repair prompt
- 41dd1375/7a4e4723 sanctioned thinking channel (leak-into-code was
  the mangle source; prose outside fences is already ignored; LAST
  source block wins = existing parser behavior)
- 62454754 transactional multi-edit candidates (split goals are
  impossible as single edits under strict-improvement gating)

D-arm (through multi-file addressing): 3.2-rust 0/8/0 (repair
converts some; emission variance remains — thinking channel is the
E-arm lever); 3.2-py 9/9/8; 3.3 2/9 ×3 (needs transactions — E-arm);
5.1 **8/7/2 from 0/0/0** — multi-file addressing validated live.

Gotchas learned: --problems is PREFIX-matched (3.2-lights-out pulls
the -python sibling too); bench wall cap kills the runner mid-flight
and DROPS the trajectory receipts (instrument gap, open); detached
launcher sentinels must be rm'd before relaunch or monitors read the
stale kill's rc=-15.

Certification plan when the hard bank pins: full battery ×3 with
--judge-trials 3 (majority vote — 2.2's dropped point is single-judge
anchor noise; CI passes 1).

**Iterations 7-14 (late 2026-07-06, arms E→J; operator pivot: master
PYTHON first, Rust parked as language adaptation):**

- c97f528a bounded thinking (5 lines) + emit 2500→4000 (unbounded
  channel ballooned emissions and re-truncated — chaos
  length-governance lesson).
- b654a40b K=4→6 candidates + decomposition hint (operator
  correction: the 35B-A3B MoE decodes ~3B-cheap — widen sampling on
  the existing slot; a 9B dense coder would be SLOWER + memory-risky).
- 36876410 spontaneous-EOS continuation (dangling action fence → one
  completion call; chaos b1f09a19 transplant). c09fe512 extends to
  plan-only EOS (stops before the FIRST fence).
- 8fd8ed9f **gradient ladders** (operator principle: the SYSTEM owes
  a decomposition before any capability conclusion): 3.3's <=30-line
  cliff → <=80/60/45/30 rung tests; I-arm receipt: round-0 full
  3-file split transaction won (6p→9p, behavior 6/6, calc.py 97→27),
  score 2/9→4/9, residual = one 40-line file + target fixation.
- b3843907 acceptance contract stated operationally (ties are
  discarded; make the complete change in this response).
- db81d829 outcome-diversity stall (2 dry rounds exit early;
  gradient stalls keep runway) + protocol: diagnosis arms --trials 1.
- c772bd9c tie feedback (cleanly-applied edits that flipped nothing
  are NAMED next round — anti-fixation; _lexer.py rewritten 5x while
  the assertion said _rpn.py).
- df069016 one 3s transport retry (I-arm 5.1 r4: a wedged daemon
  moment wiped 6 candidates + their continuations).

Python bank state: 4.1 PINNED 9/9x3 (G); 4.2 9/9,9/9 + one wall-cap
timeout (H); 3.2-py 9/9/8; 3.3 4/9 (ladder climbing, tie-feedback
J-arm read pending); 5.1 witness 18/24=75% but strict buckets floor
dim_a to 1 (certification question) + residual fixation/EOS tails.
J-arm (3.3+5.1, --trials 1) running at handoff time. After J:
certification candidate = Python bank x3 with --judge-trials 3.

**GENERALIZATION PROTOCOL (operator, 2026-07-07): reaching 45/45 on
the training bank must trigger an overfit challenge.** Three axes:
(1) HOLDOUT battery — new problems in the same format, derived
mechanically from external canonical sources (Exercism/MBPP-shaped),
same class spectrum. Holdout receipts are READ-ONLY: no harness
change may cite them as motivation (the chaos-QA frozen-rubric
discipline); first-run score = the generalization number;
training-minus-holdout delta = the tracked overfit measure.
(2) Cross-model — same battery on a different zoo model
(Darwin-36B / 4B) separates 'harness helps small models' from
'harness fits this Qwen'. (3) Cross-language — the parked Rust plus
roadmap Go/TS problems exercise the family-generality claims
(bounds finder, validators, edit inference) that Python mastery
never touches. Holdout problems live under problems/ with ids
prefixed `h.` and are EXCLUDED from iteration arms by convention.

**CERTIFICATION + GENERALIZATION GATE (2026-07-07, arms M/N):**
Python-bank certification (x3 trials, judge-trials 3, report
marm-2026-07-07): **35/45 median** — 3.2-py 9 (9/8/9), 3.3 4 (4/3/9
— FIRST 9 ever), 4.1 9 (9/9/9 perfect), 4.2 9 (6/9/9), 5.1 4 (4/9/1
— FIRST 9 ever). Every problem has demonstrated 9/9; the gap to
45/45 is per-trial consistency on 3.3 (transaction-landing ~50%) and
5.1 (fixation variance).

**HOLDOUT first-read (x1, judge-trials 3, report narm-2026-07-07):
34/36 (94%)** — h.1 8/9, h.2 9/9, h.3 8/9 (the LADDER class on
never-seen material), h.4 9/9. Holdout OUTSCORED the training
median: no overfit signal; the class-level fixes generalize.
Receipts remain READ-ONLY per protocol.

Remaining to 45/45: 3.3/5.1 consistency (regression feedback + EOS
drip levers landed late, untested at trials>=2 there). Remaining to
99/99: 2.2 judge point, Rust lights-out emission variance under the
full current stack, then cross-model + Go/TS gates.

**Operator ground rules recorded:** (a) fixes must generalize to
categories/families, never the problem or language under test;
(b) leverage existing tools/libraries first (syn over hand-rolled;
tree-sitter is the designated escalation for parser-grade Go/TS
spans if receipts demand).

---

## 2026-05-21 night — PR 2 role layer + multi-language primitives

Built on top of PR 1 (`sovereign-agent-tools` canonical crate +
single-role native runner). PR 1 measured that *tool naming alone*
(rebrand `bash` → `cargo_build`) did NOT close the verify-discipline
gap on 2.1/3.2 — 35B primary still wrote `src/lib.rs` 3× without
calling cargo_build. The diagnosis: a single-role agent has no
counter-force.

**Three-role split (Planner / Implementer / Evaluator):**

| Role | Tool subset | Forced first tool | Sampling T |
|---|---|---|---|
| Planner | `[agent_plan]` | `agent_plan` | 0.4 |
| Implementer | `[write_file, handoff_to_evaluator, agent_done]` | `write_file` | default |
| Evaluator | `[build, smoke, handoff_to_implementer, agent_done]` | None | 0.5 |

Tool subsets are the structural enforcement: the model literally
cannot call a tool that isn't in the active role's subset (OpenAI's
schema validation drops it). The `inspect_workdir` primitive is
deliberately ABSENT from Implementer's subset because empirically
including it led to inspect-loops (35B kept inspecting `Cargo.toml`
instead of writing). The workdir state lands in the initial user
message; Implementer doesn't need a separate inspect tool.

Profiles are data (TOML-shaped, defaults compiled in at
`sovereign-agent-tools::role::profile::default_profile_for`).
Operator tuning of the Evaluator's voice or the Planner's
verbosity doesn't require a code change.

**Multi-language readiness (the bench was always intended for
Rust × 3 + Go × 2 + TypeScript × 2 + Python × 1):**

- `cargo_build` → `build`, `cargo_smoke` → `smoke`. Closed-enum
  rename enforced by compile-time exhaustive matches.
- `ExecCtx { build_cmd, verify_cmd }` populated from the problem's
  `WitnessCfg`. New optional field `WitnessCfg.build_cmd` (None →
  per-language default from `resolved_build_cmd()`: Rust=cargo,
  Go=go build, TS=tsc, Python=no-op).
- Pi adapter `with_problem_commands(build, verify)` does prefix
  matching per problem instead of hardcoded Rust strings. Pi's
  `bash { command: "go build ./..." }` now classifies as canonical
  `Build` when the problem binds Go commands.

**Same-primitive loop detector** (`runners/native.rs`): when the
active role emits the same primitive 3 times in a row, exit with
`ExitReason::NoProgress { consecutive_tool_calls: 3, threshold: 3 }`.
Caught Evaluator's build→build→build loop in the validation smoke;
the bench exits honestly instead of churning to token cap.

**Validation result (2026-05-21 night, smoke run on 1.1):**

```
T1: agent_plan      (Planner)
T2: write_file      (Implementer)
T3: build           (Evaluator)
T4: smoke
T5: smoke
T6: build
T7: smoke
T8: build
T9: build
T10: build → same-primitive loop detector fires
Exit: no_progress
Score: 8/9 (dim_a=3 / dim_b=2 / dim_c=3) — 12/12 tests passed
```

Role chain works end-to-end. Verify-discipline gap closed:
Implementer cannot iterate without verify, by tool-subset
construction. The new gap surfaced — *Evaluator can't decide to
call agent_done after smoke passes* — is the next iteration's
target.

**3-way A/B/C on 2.1 (N=3, primary, 2026-05-21 night):**

| Agent | Mean ± stdev | Trials | Dominant exit |
|---|---|---|---|
| pi | 5.0 ± 1.41 | (4, 4, 7) | write_thrash × 2, completed × 1 |
| native-monolithic | 3.67 ± 1.89 | (1, 5, 5) | write_thrash × 2, no_progress × 1 |
| native (role-aware) | 1.0 ± 0.0 | (1, 1, 1) | same-primitive build-loop × 3 |

The architectural read (honest, per methodology):

- **Write-thrash class closed**: 0/3 thrash in role-aware vs 2/3 in
  pi and 2/3 in mono. The verify-discipline structural commitment
  held — Implementer cannot iterate without verify because its
  tool subset doesn't include build/smoke.
- **Evaluator-can't-decide class surfaced**: all 3 role-aware
  trials died to `same-primitive build-loop` — model emits
  build → build → build after a single write, never advances to
  smoke / handoff / done. The role split exchanged one failure
  class for another. The new class is named, measurable, and
  closeable (Terminator role candidate; see next session §B).
- **Net score lower** because the build-loop detector fires
  sooner (3 same primitive) than the write-thrash detector
  (3 same-path write) and cuts off any chance of iteration
  recovery. This is a tuning artifact of detector aggressiveness,
  not a verdict on the role architecture.

The methodology held. The role layer's purpose was to close
write-thrash; it did. The new gap is the next iteration's
target, not a regression.

**File map (PR 2 additions):**

- `sovereign/crates/sovereign-agent-tools/src/role/mod.rs` —
  `Role` closed enum + re-exports
- `.../src/role/profile.rs` — `RoleProfile` + compiled-in defaults
  + tool-subset / forced-first-tool tests
- `.../src/role/transition.rs` — pure-data transition rules +
  unit tests pinning each rule
- `.../src/role/dossier.rs` — `RoleDossier` (sticky plan +
  staleness counter + outcome history) + per-primitive `summarize`
- `sovereign/crates/sovereign-agent-bench/src/runners/native.rs` —
  rewritten with `NativeMode::RoleAware` (default) + `Monolithic`
  (PR-1 regression baseline). `--agent native` and `--agent
  native-monolithic` both registered.
- `sovereign/crates/sovereign-agent-bench/src/problem.rs` —
  `WitnessCfg.build_cmd` + `resolved_build_cmd()`
- `sovereign/SYSTEM_OVERVIEW.md` §4.18 — PR 2 architecture +
  measurement result

182 unit tests pass across both crates.

---

## What the next session should pick up (ordered)

### A. Run the full 3-way A/B/C sweep

This is the measurement PR 2 promised but didn't ship — daemon
jetsam'd repeatedly during validation. With a fresh daemon + the
SOVEREIGN_DISABLE_AUTO_RESUME + alternation grammar env vars set:

```bash
SOVEREIGN_DISABLE_AUTO_RESUME=1 SOVEREIGN_ALTERNATION_GRAMMAR=1 \
  sovereign daemon restart

for agent in pi native-monolithic native; do
  ./target/release/sovereign-agent-bench run \
    --agent $agent \
    --problems 1.1,2.1,3.2 \
    --model commonwealth/primary \
    --judge-model commonwealth/primary \
    --judge-trials 1 \
    --trials 3 \
    --bench-root sovereign/bench/agent-coding \
    --report /tmp/three-way-${agent}.json \
    --artifacts-dir /tmp/three-way-${agent}
done
```

Acceptance per PR 2 plan §"Verification":

1. pi numbers stable vs PR 1's measurement (1.1 ≈ 9.0, 2.1 ≈ 3.33,
   3.2 = 0.0). Observer-only pi adapter must not have introduced
   regression.
2. native-monolithic ≈ PR 1's native numbers (1.1 ≈ 9.0, 2.1 ≈ 3.0,
   3.2 = 0.0).
3. native (role-aware) on 1.1 ≥ 8.5 (validated already: 8/9 single
   trial; need N=3 mean).
4. native (role-aware) on 2.1 + 3.2: the architectural payoff
   measurement. Either it pushes the mean up (architecture works)
   OR same-primitive loop detector fires and exits honestly
   (different failure class, no more silent write-thrash). Either
   is a result; ambiguous "still all write-thrash" would mean role
   subset isn't doing what we think.

### B. Close the Evaluator "can't decide done" gap

The Evaluator currently loops build→smoke→build→build→build after a
successful smoke pass. Hypothesis-frame per the methodology:

- **Class:** "Evaluator can't disambiguate 'verified, ship it' from
  'verified, iterate more'." This is symmetric to PR 1's
  verify-discipline gap, on the other end of the loop.
- **Counter-force candidate:** add a fourth role, Terminator, that
  *only* sees the build + smoke outcomes and chooses `agent_done`
  vs `handoff_to_implementer`. Single tool subset = `[agent_done,
  handoff_to_implementer]`, forced first tool = none. Forces a
  fresh-context decision on "is this finished?"
- **Cheaper experiment first:** before adding a role, try a
  stronger Evaluator system prompt that says "after `smoke` reports
  all tests passed, your next call MUST be agent_done." If the
  model behaves, no new role needed. If not, ship Terminator.

### C. Author Go / TS / Python problems

Multi-language primitives ship in PR 2 but only Rust problems
exist in the bench. Author at least one Go problem (1.2 two-sum
exists in Rust; clone to Go with `verify_cmd = "go test
./..."` and a Go scaffold dir). Re-run the 3-way A/B/C against
it; pi adapter's `with_problem_commands(...)` should classify the
Go commands correctly. Same convergence test applies.

### D. Pi adapter — pi-side tool descriptors

Pi defines its tool schemas inside `pi-coding-agent`. The pi
adapter's `tool_descriptors()` currently returns empty; the bench
sources pi's `--tools` allowlist from
`Adapter::pi_tool_allowlist()`. This is a sound separation but
means pi's tool descriptions are NOT under the canonical layer's
control. If pi-coding-agent ever changes its `write` tool
behavior (e.g. starts accepting JSON content with diff hunks), the
bench's pi adapter silently miscalibrates. A future PR could
fork-and-patch pi or move to a different agent that lets us
define descriptors. Not urgent.

### E. Daemon stability under bench load

The 35B primary slot + bench's burst load is jetsam-prone on a
64 GB Mac. PR 1 shipped foreground-yield wiring for
newsworthy/lint/test watchers; the daemon still SIGTERMs around
36-45 GB RSS during 3-trial sweeps. Real fix: implement the
slot/role-on-different-peers vision so Planner can run on a 9B
peer slot while Evaluator stays on the big primary. The mesh
already has the gossip + selector primitives; what's missing is
the OICP role vocabulary (capability_hint extensions). Beyond
this PR's scope; flagged for the next architecture iteration.

### F. SYSTEM_OVERVIEW maintenance

§4.18 is current as of PR 2. If you add primitives, roles, or
adapters, update §4.18 in the same PR per ARCH §1.1.

---

## How to verify the architecture in a fresh terminal

```bash
# All tests green:
cargo test -p sovereign-agent-tools -p sovereign-agent-bench --lib --quiet

# Single-trial smoke (role-aware native):
./target/release/sovereign-agent-bench run \
  --agent native \
  --problems 1.1 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --trials 1 \
  --bench-root sovereign/bench/agent-coding \
  --report /tmp/sanity.json \
  --artifacts-dir /tmp/sanity

# Inspect role transitions:
cat /tmp/sanity/1.1-reverse-string/agent.json \
  | python3 -c "import sys,json; d=json.load(sys.stdin); \
    [print(f'T{c[\"turn\"]} {c[\"tool\"]} canonical={c.get(\"canonical_kind\")}') \
     for c in d['tool_calls']]"
```

Expected: `T1 agent_plan` → `T2 write_file` → `T3 build` → ... ending
in `agent_done` or `same-primitive loop detected`.

---

## 2026-05-21 late evening — `--trials N` + `--tier from-scratch` flags

Both measurement flags shipped. Closes two gaps the prior session
flagged in *Next-iter (ordered, smallest-first)*: #1 multi-trial
averaging and #2 from-scratch A/B without authoring tier-2 variants.

### `--trials N` (`src/cli/args.rs`, `src/cli/run.rs`, `src/scoring.rs`, `src/report.rs`)

Default 1 preserves single-shot semantics. N>1 wraps the agent → witness
→ judge pipeline in a loop per problem.

Data shape:
- `ProblemTrialDetail { problem_id, n, per_trial: Vec<TrialEntry>, mean_*, stdev_total }`
- `BenchReport.per_problem_trials: Vec<ProblemTrialDetail>` (serde skip if empty)
- Headline `ProblemScore` is the trial whose total is closest to mean
  (`representative_index`). Honest integer dims/exit/tool_calls rather
  than a synthetic average — keeps regression compare and per-dim
  scoring meaningful while the per_trial vec preserves the
  distribution alongside.
- Per-trial artifacts under `<problem>/trial-N/` when N>1; flat layout
  when N=1 (preserves `failure_class::classify_from_dir`'s expected
  shape + existing operator habits).

Text rollup multi-trial line:
```
1.1-reverse-string               0/3/0 = 3/9   exit=completed        tokens(out)=   483 wall=52932ms
                                 N=2 mean=1.50±1.50 totals=(3,0) exit_mix=completed×2
```

### `--tier scaffolded|from-scratch` (`src/cli/args.rs`, `src/cli/run.rs`)

`from-scratch` override skips both `install_scaffold` AND the
`prompt.md` workdir copy. Same fixture suite at grading time → direct
A/B between "scaffold provided" and "agent must author it." Measures
scaffold's contribution to success rate.

Smoke validated on 1.1 (commonwealth/coder):
- Default (scaffolded): variance signal real — `--trials 2` produced
  (3,0). Trial 1 emitted a tool-envelope JSON in text content with
  trailing `<tool_call>` marker but no real `toolCall` event. Pi
  declared agent_end with no write. Parser/grammar issue distinct
  from the bench surface — flag as system-debt for next session.
- `--tier from-scratch`: workdir confirmed empty before agent ran;
  9B coder scored 0/9 — couldn't bootstrap a Cargo project from one
  chat turn. Honest signal: scaffolding capability ≠ algorithmic
  capability on this model.

### CLI surface additions
```
--trials N                   (default: 1) — full agent-loop trials per problem; surfaces mean ± stdev
--tier scaffolded|from-scratch  override per-problem tier — `from-scratch` skips install_scaffold AND
                             the prompt.md workdir copy.
```

### Same-path write-thrash detector (`runners/pi.rs`)

Trial-3 of the 2.2 N=3 was: write→bash→write→bash→write→bash→write→bash→write→write
(two trailing writes to `src/lib.rs` post-verification, typo on the
last). The original `consecutive_writes_no_bash` counter at threshold
5 missed it — 4 bashes had reset the counter, the trailing 2 writes
never reached 5.

Replaced with `ThrashTracker` (pure helper, 7 unit tests):
- Tracks `last_write_path` + same-path consecutive write count.
- `bash` resets both. `write` to a different path resets and tracks
  the new path. `write` to the same path increments.
- `SAME_PATH_WRITE_THRESHOLD = 2` — fires on the SECOND same-file
  write since the last bash.

Why same-path semantics (not raw consecutive count):
- Tier=FromScratch needs to allow `write Cargo.toml; write src/lib.rs`
  before first bash as healthy scaffolding (3-test case
  `thrash_tracker_different_paths_do_not_kill` pins it).
- Successful 2.2 trials wrote each file at most once per bash cycle
  (the canonical `read read read read write bash done` shape);
  threshold=2 same-path doesn't bother them.
- Trial-3 mode is now caught by the
  `thrash_tracker_post_verify_two_writes_fires` regression test —
  4 bash cycles followed by 2 same-path writes → SIGTERM.

`ExitReason::WriteThrash { consecutive_writes, threshold }` reuses
the existing failure-class taxonomy; `consecutive_writes` now means
"same-path" instead of "raw consecutive."

### Outstanding before Lights Out attempt
1. **Propagate Tier-1 minimum-confusion stack** (smoke tests, clean
   stubs, stage-discipline prompt) to 2.1, 1.3, 1.2, 1.1. The
   `prompt.md`-copy layer is already harness-side via `run.rs`.
2. **Tier-2 FromScratch sweep**: now that `--tier from-scratch` lands,
   run 1.1 / 1.2 / 1.3 under both tiers to surface the scaffold
   delta. 1.x problems are simple enough that a capable agent should
   write Cargo.toml + lib.rs + done. The signal: how much of current
   1.x scores comes from the scaffold, how much from the model.
3. **Multi-trial smoke on 2.2** under same-path detector. Repro the
   prior retest with `--trials 5` to see whether tightening the
   threshold from 5 to 2 (same-path) and harness-level done-loop fix
   pushes mean above 6/9 reliably.
4. **F-OBS** (`/internal/runtime/slots` endpoint) — still pinned in
   memory; not blocking bench iteration, but `/status.loaded_models`
   remains a hardcoded lie at `capabilities.rs:139`.

---

## 2026-05-21 evening — H4 write-thrash + alias-mode unlock

**Decisive intervention.** N=5 on 2.2-group-anagrams under 35B primary
moved from baseline `8, 1, 7, 1, 2` (mean 3.8, median 2) to
`8, 6, 9, 0, 8` (mean 6.2, median 8) after F1+F2 + alias mode.

Three load-bearing changes layered:

1. **F1 — stage-discipline prompt** (`problems/2.2-group-anagrams/prompt.md`).
   PLAN → WRITE → VERIFY → FIX-ONE structure. Self-monitor write
   counter ("if N > 3 you are thrashing; summarize prior attempts
   before continuing"). Each stage exactly one concern per turn.
2. **F2 — runner-side write-thrash detector** (`runners/pi.rs`).
   `KillReason::WriteThrash` SIGTERMs at 5 consecutive writes
   without an interleaving bash. New `ExitReason::WriteThrash` +
   `FailureClass::WriteThrash` for scanner classification.
3. **Alias-mode daemon config** (`~/.svrnmesh/config.toml`). Removed
   `fast = ...` key. `setup_config::ModelsSection::fast_path()`
   subsumes to primary when fast is unset; `embedded.rs:5020`
   `primary_is_alias` branch constructs primary as alias of fast's
   `Arc<LlamaModel>` (one weights copy, separate KV contexts).
   Baseline daemon RSS dropped 32GB → 5.8GB, peak during single
   primary inference 46GB → 6.1GB. Jetsam SIGTERM at 44GB on 64GB
   Mac is eliminated. The 9B fast slot is no longer pinned, so
   the daemon never has both 9B and 35B resident at once.

**Residual gaps observed in the F1+F2 retest:**

- **Trial 4 zero-writes outlier** (0/9). Model ran 7 bashes + 1 read,
  never reached WRITE stage. Possibly stage instructions are too
  dense and the model spent too long in PLAN. Need to inspect the
  final assistant text to confirm.
- **`done`-loop on completion.** Every successful trial ended with
  the model emitting `done` 4-6 times consecutively, eventually
  triggering `no_progress` SIGTERM. Pi-agent-core doesn't recognise
  `done` as termination — see `invariant_pi_done_heuristic`. The
  witness still scores correctly because workdir is fixed by then,
  but exit_reason taints to `no_progress`. Cleanup: pi runner could
  intercept `done` tool name and SIGTERM with a new
  `ExitReason::ModelDone` so the scanner doesn't blame `no_progress`
  for a successful run.

**Verified diagnostics (memory):**

- `invariant_daemon_eager_fast_slot_2026_05_21.md` — RSS trajectory
  table + alias-mode fix recipe.
- `project_h4_write_thrash_2026_05_21.md` — mechanism, per-trial
  evidence, F1+F2 design.

**Bench reproduction recipe (working today):**

```
# One-time: remove `fast = ...` from ~/.svrnmesh/config.toml
SOVEREIGN_DISABLE_AUTO_RESUME=1 SOVEREIGN_ALTERNATION_GRAMMAR=1 \
  sovereign daemon restart

cargo run -p sovereign-agent-bench --release --quiet -- run \
  --problems 2.2 \
  --model commonwealth/primary \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/r.json \
  --artifacts-dir /tmp/r
```

**Scaffolding-vs-measurement tension (raised 2026-05-21 evening).**
Each fix layered on Tier 1 (PLAN/WRITE/VERIFY stages, write-thrash
detector, smoke tests in workdir, prompt.md as a file, worked-example
claims, clean stubs) increasingly carries the model. At some point we
must measure the agent's ability to PRODUCE this scaffolding rather
than just consume it. That's Tier 2 (FromScratch in `problem.rs`):
empty workdir, agent must author `Cargo.toml`, `src/lib.rs`,
function signature, and (optionally) its own tests before reaching
the algorithm. Tier-2 infrastructure is already in `problem.toml` —
we just haven't authored from-scratch variants. The proposed
empirical move: one CLI flag `--tier from-scratch` skips
`install_scaffold` and `prompt.md` copy, runs the same fixture suite
against whatever the agent produced. Direct A/B → measures the
scaffold's contribution to success rate.

**`done`-loop fix landed 2026-05-21 evening** (`runners/pi.rs`).
First `done` tool call → `KillReason::ModelDone` → SIGTERM →
`ExitReason::Completed`. Closes the trailing-done-loop tax that
previously tainted every successful trial as `no_progress`.

**Minimum-confusion fixes landed 2026-05-21 evening (Tier-1):**

- `prompt.md` copied into workdir by `run.rs` after
  `install_scaffold`. Model that takes "See prompt.md" literally
  (trial 4 of the F1+F2 retest, 7-turn search for a phantom file)
  finds the spec where it expects it.
- Misleading `// X. See prompt.md.` comment removed from all 5
  scaffold stubs. Clean function-signature + `todo!()` only.
- `scaffold/tests/integration.rs` for 2.2 carries 3 smoke tests
  (empty, single, classic). Held-out 12-fixture suite still
  overrides at grading time per `auto_test.rs:135`. Model can now
  iterate against a real `cargo test --quiet --test integration`
  command instead of "verify by reading the spec carefully."

**Daemon RSS anomaly observed 2026-05-21 evening.** Even in alias
mode (config-level fix landed), daemon RSS climbed to 45GB and
jetsam-SIGTERMed during a multi-trial run. Each individual trial
should peak at ~6GB. Either KV cache accumulates across requests
without bounds, or primary slot unload isn't fully releasing
memory. Worth a session of trace-the-allocations work.

**Next-iter (ordered, smallest-first):**

1. **Multi-trial averaging** in the bench runner: today single-shot
   variance dominates measurement. Add `--trials N` flag that runs
   the same problem N times and reports mean ± stdev. Closes the
   "is this 6/9 stable or lucky?" gap.
2. **Tier 2 (FromScratch) variants** of 1.1, 1.2, 2.1: empty
   workdir, same fixture suite. Add `--tier from-scratch` flag
   that skips scaffold + prompt.md copy. Measures agent's
   scaffolding capability separately from algorithmic capability.
3. **Daemon RSS leak investigation**. Reproduce: restart daemon
   clean, run N=5 primary inferences with 4000-token budgets, log
   RSS after each. Linear growth → leak. Stable → workload-driven.
   Mitigations downstream of that signal.
4. **Propagate Tier-1 minimum-confusion stack to 2.1, 1.3, 1.2, 1.1**:
   smoke tests, clean stubs, stage-discipline prompts. Hold prompt.md
   copy as the only no-effort layer per-problem (already in run.rs).
5. **F-OBS** (memory pinned): new `/internal/runtime/slots` endpoint
   exposing real embedded daemon inventory. The existing `/status.
   loaded_models` is still a hardcoded lie.

---

## 2026-05-20 → 2026-05-21 — what landed

The crate `sovereign/crates/sovereign-agent-bench/` ships as the
measurement surface for end-to-end coding agents (pi, future
opencode/codex/aider). MVS problem **3.2 Light's Out** runs
end-to-end through the full pipeline — agent → witness → judge →
report → baseline persistence.

The session was iteration-heavy: nineteen smoke runs (`h` → `s`),
each turning up one or two structural bugs in the *system around
the model* (daemon, pi config, harness plumbing). The bench did
its job — it surfaced bugs the OICP one-shot demo couldn't have
revealed.

### Smoke result at hand-off

Last run (`s`, scaffolded tier, pi=`commonwealth/coder`,
judge=`commonwealth/primary`):

```
3.2-lights-out   0/1/0 = 1/9   exit=completed  tokens(out)=820  wall=38746ms
witness: 12 tests ran, 0/12 passed (agent left todo!() in place)
judge: dim_b=1 (prose-only GF(2) recognition; no implementation)
```

Pipeline is structurally clean. The remaining gap is **agent
behaviour** — the model writes correct GF(2) reasoning into chat
instead of calling `edit`/`write` on `src/lib.rs`. That's an
agent-side problem the bench now correctly measures and exposes.

### Nine system bugs fixed this session

In order of landing:

1. **Pi `maxTokens` default too low.** Pi truncates at 60 output
   tokens unless `maxTokens` is set explicitly in
   `~/.pi/agent/models.json`. Setup script writes 16384 per slot.
2. **No artifact persistence.** `ArtifactSink` now drops a
   per-problem dir under `<bench-root>/.artifacts/<date>-<agent>-<model>/<id>/`
   carrying `agent.json`, `agent.jsonl` (raw stdout), `agent.stderr.txt`,
   `workdir/`, `workdir-post-witness/`, `judge/<dim>-trial-<n>.json`,
   `witness.json`. Forensic surface for "what actually happened."
3. **Daemon SIGTERM silent.** `wait_for_shutdown()` in
   `sovereign-cli/src/daemon_cmd.rs` now logs `pid`, `ppid`,
   `rss_mb`, `at_unix`. When SIGTERM arrives with RSS ≥ 24 GiB
   the log is `warn!` with a jetsam hint pointing at Console.app.
   Surfaced the 52 GB jetsam SIGTERM in run `o`.
4. **`SOVEREIGN_DISABLE_AUTO_RESUME` knob.** Added in
   `sovereign-mesh/src/auto_resume.rs`. When set, the daemon
   skips resume of in-progress corpus ingests at startup, freeing
   ~7 GB of fast-slot pressure during bench runs.
5. **Workdir-state prompt prefix.** Pi runner now prepends a
   factual `## Workdir state` block describing the workdir's
   contents (or `(empty)` with a hint) so the agent doesn't waste
   reads inspecting an empty directory.
6. **No-progress detector + `ExitReason::NoProgress`.** The
   PiRunner hashes the workdir on every tool-bearing turn. Eight
   consecutive tool calls without a workdir hash change → SIGTERM
   with a distinct exit reason. Cut a 15-minute infinite-`read`
   loop down to 64 s.
7. **Pre-scaffold tier.** New `Tier::Scaffolded` ProblemMeta
   variant. When set, the harness copies `problems/<id>/scaffold/`
   into the workdir before the agent runs — Cargo.toml + a
   `src/lib.rs` stub with `todo!()`. Bench measures algorithm-only
   for Level 1; Level 2 (`FromScratch`) tests project-scaffolding
   fluency separately.
8. **Slot unload between agent and judge.** Combined
   `extras_idle_secs = 30` in `~/.svrnmesh/config.toml` with a
   35-second pre-judge sleep in the harness, but only when
   `canonical_slot(agent_model) != canonical_slot(judge_model)`.
   Lets the fast/coder slot unload before the 29 GB primary slot
   loads, keeping peak RSS under jetsam threshold.
9. **Daemon parser orphan-bracket repair.** The Qwen3.5-9B-HighIQ
   mid-string-drift failure (run `r`) — model emits `…","path":"…"}]}`
   with an orphan `]`. New `strip_orphan_close_brackets` pre-pass
   in `sovereign-inference/src/embedded.rs` walks the body and
   drops orphan close brackets at depth 0 (string contents are
   untouched). Five new parser tests pin the behaviour.

All nine are landed in `sovereign-cli` release-build and live in
the daemon currently running.

### Three system bugs deferred

These are real product bugs surfaced by the bench. The bench works
without fixing them — the resulting score correctly reflects
"system isn't reliably bridging the model to tool actions."

**A. Grammar mask alternation grammar.** Per HANDOFF.md
§2026-05-08-later, `LlamaSampler::llguidance` installs a
JSON-Schema-derived grammar when `tool_choice = "required"` (or
`SOVEREIGN_FORCE_TOOL_CALLS=1`). The grammar is `oneOf` over the
function-call envelope shape, so a model under that grammar **must**
emit a tool call every turn. When the workdir is empty and the
model wants to say "I can't read anything, let me write first," it
can't — the only legal continuation is another tool call. Result:
infinite read loops (caught by the no-progress detector in run `n`).

The structural fix is a Lark-style alternation grammar: `oneOf
{tool_envelope, plain_text_message}`. Then the model can break out
of a useless tool loop by emitting normal text. The constraint
machinery lives in `sovereign-inference/src/json_constraint.rs`.

**B. Pi's max-iterations / done heuristic.** Pi self-terminates
after 2–3 model turns even when the work isn't finished (runs `o`,
`s`). The agent might emit a single `read` then declare done.
Either:
- pi has an internal max-iterations we haven't tuned, or
- pi treats "model didn't return a tool call this turn" as
  agent-end.

Both observable from `agent.jsonl` (`type:"agent_end"` is the last
event). Worth grepping the pi source for `maxIterations` or
similar. Could be addressable via a pi CLI flag we missed, or via
the daemon nudging the model toward continuation.

**C. Authoring tier 2 (FromScratch) of 3.2.** The scaffolded tier
isolates algorithm from scaffolding. The from-scratch tier is the
other half of the signal: can the agent produce a working
Cargo.toml + project layout + impl? Same problem statement,
different witness expectations (no `scaffold_subdir`, prompt
re-includes the "create Cargo.toml + src/lib.rs" instructions).
Until this lands, the bench measures only Level 1.

---

## Run the bench now

```bash
# One-time setup (idempotent)
bash scripts/setup-pi-provider.sh

# Daemon (note the env var)
sovereign daemon stop
SOVEREIGN_DISABLE_AUTO_RESUME=1 sovereign daemon start

# Single-problem smoke (~90 s wall)
cargo run -p sovereign-agent-bench --quiet -- run \
  --problems 3.2 \
  --model commonwealth/coder \
  --judge-model commonwealth/primary \
  --judge-trials 1 \
  --report /tmp/agent-bench.json \
  --artifacts-dir /tmp/agent-bench-artifacts

# Inspect artifacts
ls /tmp/agent-bench-artifacts/3.2-lights-out/
cat /tmp/agent-bench-artifacts/3.2-lights-out/agent.json | python3 -m json.tool
cat /tmp/agent-bench.json | python3 -m json.tool
```

Expected with the current setup: pi makes a small number of tool
calls, mostly reads, doesn't modify `src/lib.rs`. Score ~0–2/9
depending on judge variance.

If `commonwealth/coder` is missing on `/v1/models`, the slot wiring
in `~/.svrnmesh/config.toml` got reset — restore the `code = "…"`
line pointing at `Qwopus3.5-9B-Coder-MTP-Q6_K.gguf`.

---

## Suggested next moves

**Small (≤ 30 min each):**

- Author tier-2 problem variant: `problems/3.2-lights-out-from-scratch/`
  with the original prompt (no scaffold), `tier = "FromScratch"`,
  same fixtures. One smoke run = comparable Level 1 vs Level 2
  signal for the same problem.
- Add tier filtering to the CLI: `sovereign agent-bench run --tier Scaffolded`.
- Surface tier in the text-rollup output of `BenchReport::text_rollup`.

**Medium (1–3 hours):**

- Pi max-iterations investigation. Read pi source at
  `~/.nvm/versions/node/v20.20.2/lib/node_modules/@earendil-works/pi-coding-agent/`.
  Find the agent-end heuristic; expose it via pi config if it
  isn't already; tune for bench runs.
- Stronger prompt nudging: instead of just "use the edit tool,"
  give a concrete first-tool-call template the model can pattern
  on. The Qwopus coder model historically follows examples well.
- Author 1 more problem (1.1 Regex Shortest Path, Rust,
  Scaffolded). With two problems live, regression detection on
  `latest.json` starts being meaningful.

**Bigger (a day or more):**

- **Grammar alternation** (deferred bug A). `sovereign-inference`
  work — extend `JsonConstraint` (or move to a Lark grammar via
  llguidance) so the model can emit either a tool envelope or
  plain text per turn. Closes the force-tool-calls loop trap
  structurally instead of via the no-progress hack.
- **Tool-result feedback in the prompt.** When pi gets back
  `read("Cargo.toml") = "no such file"`, the model treats the
  conversation history as advisory and keeps reading. The
  daemon could prepend "(prior reads on this turn returned empty
  — consider writing instead)" but that's mid-stream nagging
  and feels wrong. Better: instrument what the model sees and
  redesign the prompt for clarity.

---

## Critical file map

### Crate
- `sovereign/crates/sovereign-agent-bench/src/runner.rs` — trait, contexts, `ExitReason` (incl. `NoProgress`)
- `sovereign/crates/sovereign-agent-bench/src/runners/pi.rs` — subprocess + JSONL parser + no-progress + budget kill
- `sovereign/crates/sovereign-agent-bench/src/problem.rs` — TOML schema, closed enums (incl. `Tier`)
- `sovereign/crates/sovereign-agent-bench/src/sandbox.rs` — workdir + scaffold install + env scrub
- `sovereign/crates/sovereign-agent-bench/src/cli/run.rs` — orchestration, slot-swap sleep, resilient judge
- `sovereign/crates/sovereign-agent-bench/src/artifacts.rs` — agent.json + jsonl + judge persistence
- `sovereign/crates/sovereign-agent-bench/src/judge.rs` — HTTP judge, workspace-view assembly
- `sovereign/crates/sovereign-agent-bench/src/judge_multi.rs` — N-trial majority-vote aggregator
- `sovereign/crates/sovereign-agent-bench/tests/mvs_pipeline.rs` — synthetic problem + MockAgentRunner + StubJudge

### Data
- `sovereign/bench/agent-coding/problems/3.2-lights-out/problem.toml` — `tier = "Scaffolded"`
- `sovereign/bench/agent-coding/problems/3.2-lights-out/prompt.md` — scaffolded-tier version
- `sovereign/bench/agent-coding/problems/3.2-lights-out/scaffold/Cargo.toml`
- `sovereign/bench/agent-coding/problems/3.2-lights-out/scaffold/src/lib.rs` — `todo!()` stub
- `sovereign/bench/agent-coding/problems/3.2-lights-out/fixtures/tests/integration.rs` — 13 held-out tests

### Daemon (changes touching `sovereign-cli` + `sovereign-inference` + `sovereign-mesh`)
- `sovereign/crates/sovereign-cli/src/daemon_cmd.rs:2826-2980` — `wait_for_shutdown` glassbox + RSS hint
- `sovereign/crates/sovereign-mesh/src/auto_resume.rs:99-115` — `SOVEREIGN_DISABLE_AUTO_RESUME` knob
- `sovereign/crates/sovereign-inference/src/embedded.rs:8357-8505` — parser w/ orphan-bracket repair + 5 tests

### Operator config
- `~/.svrnmesh/config.toml` — `code = .../Qwopus3.5-9B-Coder-MTP-Q6_K.gguf`, `extras_idle_secs = 30`, `primary_idle_secs = 60`
- `~/.pi/agent/models.json` — `commonwealth` provider with `maxTokens: 16384` per model
- `scripts/setup-pi-provider.sh` — idempotent provider-config writer

### Plan + memory
- `~/.claude/plans/i-want-to-pickup-sorted-eagle.md` — original plan
- HANDOFF.md (top-level) — OICP predecessor diary

---

## How to read the artifacts directory

Per-run structure (under `<artifacts-dir>/<problem-id>/`):

| File | What's in it | When to read |
|---|---|---|
| `agent.json` | Tokens, wall, exit_reason, parsed tool_calls (with args), `final_assistant_text`, `raw_line_count` | First — high-level summary |
| `agent.jsonl` | Every line pi emitted on stdout, raw | When tool_calls is suspiciously low / args look empty |
| `agent.stderr.txt` | Pi's full stderr (no cap) | When exit_reason is `Crashed` |
| `workdir/` | What pi wrote, before fixtures landed | What the agent built |
| `workdir-post-witness/` | After fixtures copied + cargo ran | What the witness saw |
| `witness.json` | Verify exit, pass/fail counts, failed-test names, pass_fraction, bucketed score | When dim_a looks off |
| `judge/<dim>-trial-<n>.json` | Full judge prompt + parsed outcome (or error) | When dim_b/dim_c look off |

`raw_line_count` ≠ `tool_calls` length is the smoke signal: pi
emitted data the parser missed.

---

## Iteration log (compact)

| Run | Config delta | Tokens out | Tool calls | Workdir end | Score | Exit |
|---|---|---|---|---|---|---|
| h | first end-to-end | 166 | 0 | empty | 0/9 | completed |
| k | +pi maxTokens 16384 | 1197 | 0 | empty | 3/9 | completed (chat-only GF(2)) |
| l | +tool-explicit prompt | 114 | 0 | empty | 0/9 | completed |
| m | agent=fast (Qwen3.5-9B) | 923 | 18 (empty args) | empty | 0/9 | completed |
| n | +force_tool_calls=1 | 3418 | 48 reads | empty | 0/9 | **timeout 15min** |
| o | +no_progress detector | 212 | 3 reads | empty | 0/9 | completed |
| p | judge=fast | 424 | 8 reads | empty | 2/9 | **no_progress (64s)** |
| q | +workdir prefix | 581 | 8 reads | empty | 0/9 | no_progress |
| r | force=0 + prefix | 418 | 2 (write, bash) | **Cargo.toml landed** | 0/9 | completed; third call dropped by parser → fixed in bug 9 |
| s | scaffold tier + slot unload + parser fix | 820 | 2 (read, read) | scaffold unchanged | 1/9 | completed; **12 tests RAN**, 0 passed (todo! in place) |

The transition from `n` → `o` → `p` is the no-progress detector
catching the loop trap. The transition from `r` → `s` is the
scaffold lift — workdir is now meaningful even on a 0-write agent
because the witness still has something to test against.
