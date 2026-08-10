# The Comaintainer — govern the agent pool with a trained role, not a longer prompt

**Date:** 2026-08-05
**Status:** moonshot design, pre-implementation. Every count below was
measured on this date from this repo's own history; every recorded
failure is cited to the document or note that records it.

← companions: `.claude/CLAUDE.md` (the accreted constitution this doc
proposes to shrink), `sovereign/ARCH_PRINCIPLES.md` (the values; §18 is
this doc's judgment core), `sovereign/docs/EPISTEMIC_STATE.md` (the
pattern being repeated: replace prose with a typed object — there the
answer, here the verdict), `gym/next-edit/golden/` (the mining
precedent), `sovereign/DEFAULTS_LEDGER.md` (the promotion mechanism).

---

## 1. The operator's journey — the product bar, traced

The end user of this feature is the operator. Everything derives from
their day. Each scene states what happens today (cited) and the one
change the comaintainer makes at that hop (with the section that
specifies it). The numbers are this repo's, measured 2026-08-05: 170
sessions and ~2,600 operator messages over four months (~15 per
session), 1,611 commits (~12.7 landed per day), 107 live session
frames at boot.

**Scene 0 — morning, choosing what matters.** Today: the operator
picks which of 107 live threads to push, from memory; the frame index
serves successor *sessions*, and no surface answers "what needs ME
today." With the comaintainer: the day opens with one briefing —
orders in flight, overnight verdicts with overrides pending review,
escalations queued, curation delta — rendered from surfaces that all
exist and currently have no reader (§2): the verdict log, the atlas,
the frames, the ledger. The operator answers escalations and states
new intent. (§4.3 Briefing.)

**Scene 1 — intent becomes an order.** Today: intent is typed into a
fresh worker session, which re-derives context; under-specification is
repaid mid-course, ~15 operator messages per session, and the
recurring corrections recur across sessions — 31 feedback memories
exist because the same guidance kept being worth writing down. With
the comaintainer: a five-minute intake interview pins the order —
objective at initiative altitude, falsifiable done-when,
not-worth-continuing-if, measurement lane, scope, budget — moving the
correction to t=0, where it costs one exchange instead of fifteen.
Technical facts: the objective contract already exists and is enforced
(`session_state` rejects goal writes without an objective,
`SESSION_CONTINUITY §2.1`); the order file extends the ATOS feature
trail (§14.4); the lanes exist (`sovereign/bench/README.md`). Missing
only: the author. (§10 artifact 4.)

**Scene 2 — the order meets the pool.** Today: the operator launches
sessions by hand per machine and is the only collision detector; this
session booted with 173+ files under concurrent peer edit and an
advisory banner; the recorded cost of no arbiter includes a 2026-08
session that rebuilt a bench suite that already existed. With the
comaintainer: workers boot with the order plus the ten principles —
the boot hook that injects frames today injects orders, same mechanism
— scope is claimed from the order's scope block (`declare_scope`
exists), and landing order across concurrent orders is chosen with
`blast` and the layer map. (§4.3 Intake, In-flight.)

**Scene 3 — while the operator is away.** Today: a worker hitting a
judgment question either guesses (drift) or interrupts the operator —
the ~2,600 messages are the ledger of intents, answers, and
corrections interleaved. With the comaintainer: workers ask the role
first. The routing knowledge is already codified (the constitution's
"which door" table); it needs a holder, not new content. Only product
calls queue for the operator. (§4.3 In-flight.)

**Scene 4 — a worker says "done." The crux of the journey.** Today,
at ~12.7 commits/day, the operator has two options: audit every
landing, which does not scale past a single-digit daily rate, or
trust — and §18 was distilled from 818 notes precisely because trust
kept being spent on green that was not real. With the comaintainer
there is a third option: **delegate review to a role with a measured
error rate.** The seat runs the §18 audit and the smell sweep, issues
one typed verdict with citations — and at M0 that verdict reaches the
operator as a **supervised draft**: the director shows its reasoning
and citations, the operator approves or edits, and the (draft, final)
delta is logged as a training episode (`co-directive-log.sh`). The
operator reads a verdict line instead of a diff; editing it is one
command, and every edit teaches the role. (§4.3 Landing, §10
artifacts 1–4; what "measured" means is §6.4's gym.)

**Scene 5 — between times.** Today: notes, frames, ledger rows and
drift decay unless someone volunteers — drift was stale at this
session's boot, and `notes rationalize` is nobody's job. With the
comaintainer: curation is scheduled, and the briefing carries the
delta, including ledger rows past review-by, which are the operator's
call by construction. (§4.3 Curation.)

**Scene 6 — the end of the path.** The journey ends where the product
does: a Sovereign end-user gets the feature, and the operator's day
contained decisions, not relays. The journey metrics this initiative
moves, with baselines computable from history: operator messages per
landed commit (~1.6 today), operator attention per landing (a diff
today; a verdict line at L1), collision and duplicate-work incidents
(recorded but uncounted today; counted from P3's log on), override
rate over time (the role improving, §6.5), and the per-session
constitution tax (55KB today, §8).

The bar under the journey: the operator's interface to the pool is one
role whose judgment is **measured, not trusted** — cold-started from
the repo's own case law, scored in a gym the way next-edit was scored
(§3), promoted rung by rung as DEFAULTS_LEDGER rows (§7), eventually
distilled onto local weights (§6.6): the Sovereign thesis pointed at
its own development process. Physically the role is four inspectable
artifacts — charter, gym, seat, order — and every phase is an
operation on them (§10).

## 2. Root cause — governance has writers but no reader

The code-intelligence and coordination tools "haven't stuck." The
pattern behind that is uniform: **each one produces a surface addressed
to a maintainer's attention, and maintainer attention is currently
sliced into 170 transient sessions.** Receipts:

| Surface | What it produces | Who consumes it today |
|---|---|---|
| Notes store | 3,417 durable notes: 2,804 decision, 276 invariant, 217 reflection, 62 todo, 29 attempt (plus 2,896 auto tool-telemetry) | relevance-injection per turn; recall depends on the right query at the right moment. `svrn notes rationalize` exists and is nobody's job |
| Work atlas | claims + edit observations | advisory; this session booted with 173+ files under concurrent peer edit and no arbiter |
| Session frames | 131 session dirs; 107 live frames at boot, "in-flight" on six of the top eight | successor sessions only. 2026-07-29 audit: 21 of 63 frames restated objectives as deltas; one chain lost its own objective's name (`SESSION_CONTINUITY.md §2.1`) |
| Drift | narrative-vs-code findings | stale (2d) at this session's boot; refresh cadence is nobody's job |
| DEFAULTS_LEDGER | dark-ship rows with review-by dates | exists precisely because flip conditions withered in session summaries (operator directive 2026-07-31); enforcement is "if you happen to touch the area" |
| cache-audit | per-session context spend | "run it on yourself when a task ran long" — self-audit under task pressure |
| Contract census, bench baselines, posture | proof state per subsystem | run "when you touched X" |

Every row is an instrument with no staffed operator. The constitution
compensates in prompt-space: `.claude/CLAUDE.md` is 55KB (~14k tokens)
injected into **every** session, and its own texture is the evidence
that this saturates — section after section opens "this exists because
X kept happening": the compass kept getting lost, nine Reads of one
file in one session (2026-05-12 audit), a 2026-08 session that rebuilt
a bench suite that already existed, 21/63 frames drifting off their
objectives, watchers misdiagnosed at boot.

ARCH_PRINCIPLES §7 says: **make invariants structural, not
remembered.** The repo has applied that principle to every invariant
except the maintainer itself, which still lives as 14k tokens of
remembered instructions handed to every worker. The comaintainer is §7
applied to governance: stop asking every transient session to also be
the maintainer; give the duties a principal.

## 3. Why trainable, and why now

This repo already executed the exact move once, at the code level.
`gym/next-edit/golden/` mined real editing history into 1,098
stratified cases, and the measurement overturned the vibes verdict
(sweep-1.5b: "90% useful / 0% wrong" became 36% useful / 21% wrong /
60% missed). And §18 of ARCH_PRINCIPLES *is itself* a distillation run
— "clustering 818 working notes written across six months," every rule
cited to note ids. Mining history into judgeable cases, and distilling
notes into doctrine, are both proven in-house. What has never been
built is the loop that connects them to a role.

The substrate, measured 2026-08-05:

- **git:** 1,611 commits since 2026-03-31; 723KB of commit prose
  (~180k tokens — the messages here are decision documents, not
  labels); 218 `fix:` commits; **60 commits explicitly recording
  reject / revert / overturn / withdraw** — each one a verdict with its
  rationale attached ("measured and rejected — no speedup on the
  vault, worse typing"; "a baseline from another model is
  incomparable, not a regression").
- **notes:** 3,417 durable notes in the kinds above. Invariants and
  attempts are pre-labeled negative space: things that must never be
  done and things that were tried and failed.
- **transcripts:** 170 sessions, 611MB, ~2,600 operator text turns —
  every course-correction the operator ever issued is in there, in
  situ, with the full context the agent had when it went wrong.
- **distillates:** 31 feedback memories (corrections already boiled
  down), the DEFAULTS_LEDGER, CLAUDE.md, ARCH_PRINCIPLES (18
  sections), SESSION_CONTINUITY, BENCH_LOOP, RUNBOOK §6.

Constitution: exists. Case law: exists, in volume, much of it settled
by later *measurement* rather than opinion — which is what makes it
trainable ground truth rather than preference data. What does not
exist: the case law reshaped into **episodes** a candidate can be
scored on, and the **gym** that scores it.

## 4. The role, structured

### 4.1 Verdict vocabulary

Judgment is a typed object, not prose (the EPISTEMIC_STATE move). Six
verdicts:

- `approve(citations)` — meets the bar; names which gates proved it.
- `revise(basis, ask)` — `basis` is a smell-table row or a principle §;
  `ask` is the concrete change.
- `measure-first(instrument)` — the claim is unproven; names the bench
  lane, shadow flag, census, or soak that would prove it (§18.4:
  validate the instrument before the result; §18.5: one run is not a
  measurement).
- `split(scopes)` — the order crosses claims or concerns (§14.1).
- `escalate(question)` — genuinely operator-owned: product priority,
  taste, budget, privacy.
- `could-not-judge(missing)` — honored as a first-class verdict
  (§18.2: four verdicts, not two). A comaintainer that cannot say this
  will fabricate confidence; §18.3 applies to governance exactly as it
  applies to inference.

### 4.2 Citation obligation

Every non-approve verdict names its basis: an ARCH_PRINCIPLES §, a
note id, a ledger row, or a measurement. This is §11 (cite, don't
recall) applied to judgment — and it is what makes verdicts scoreable
beyond mere agreement: citation validity (the anchor exists and bears
on the case) is mechanically checkable.

### 4.3 Duties by phase

**Intake.** Operator intent → work orders. Order schema: objective
verbatim at initiative altitude with done-when and
not-worth-continuing-if (the `SESSION_CONTINUITY §2.1` contract,
already specified and already enforced by `session_state`), scope to
be claimed via `declare_scope`, the measurement lane that will prove
the work, the ENGINE (model + effort, per phase — the operator's
bank-frame-and-restart dexterity as an order field; the seat
recommends from task shape, the operator's edits are the taste it
learns), a budget, and the seam contracts the worker must not
renegotiate. Workers boot with **the order plus the ten principles**,
not the 14k-token constitution — the comaintainer holds the rest.
§14.4's ATOS feature artifacts (`.sovereign/features/<id>/`) are the
existing per-feature trail this extends, not a new invention.

**In-flight.** Watches the atlas; arbitrates collisions instead of
letting the boot banner advise into the void; answers workers' "which
door do I open" questions (it holds the routing table so they don't);
reads `cache-audit` on long-running workers and intervenes on
raw-acquisition drift. The safety switch (operator directive
2026-08-06, as-built in `.claude/skills/comaintainer`): at a worker's
yellow cutoff the seat reads its freshly-banked frame and diffs it
against the order (objective verbatim, next-vs-done-when, budget); at
the hard cut no worker splits or respawns without the operator's ack
through the seat — it parks instead. Worker claims are verified with
the code tools before they are relayed or judged (§11 applied to
reports); the seat holds the forest, descends only to verify.

**Landing.** Mechanical gates stay code and run first — lint, test,
census, the bench lane named in the order. §7.6: never ask a model to
guarantee what code can enforce. The comaintainer *reads* those
results, then runs the judgment pass code cannot run — the §18 audit:

| Implicit claim in every landing | The rule it answers to |
|---|---|
| "the tests prove it" | §18.1 — was the gate ever watched to fail? does it assert on something the subject cannot author? |
| "the suite is green" | §18.2 — any could-not-judge or never-ran collapsed into a pass? zero-test green? |
| "the fallback is fine" | §18.3 — any `Err` collapsed into a success shape? absence defaulted? |
| "the bench says improved" | §18.4/§18.5 — instrument validated? same evidence the system consumed? more than one run? baseline existed before the run? |
| "docs updated" | §1.1 — same commit, or a drift finding waiting to happen |
| "decision recorded" | §14.2 — note at the moment of decision; ledger row if anything shipped dark |

plus the smell-table sweep (§15) and landing-order selection across
concurrent orders using `blast` and the layer map. This judgment pass
is precisely the mineable skill: the history is full of labeled
examples of green-that-wasn't.

**Curation (the gardener).** The unstaffed instruments get a staff:
ledger review-by sweeps, frame retirement (107 live frames is a
backlog, not a state), `notes rationalize` runs, drift refresh
cadence, bench-baseline coverage (`svrn posture`), fleet-report
reading.

**Briefing.** Upward reports in the house voice. Escalations carry a
decision to make, never a dump to read.

### 4.4 Boundaries

The comaintainer **never writes feature code** — judge, not player;
its context stays judgment-shaped and it never competes with the pool
for work. It **never self-amends its charter** — amendments are PRs
the operator approves (§17 already sets this norm for
ARCH_PRINCIPLES). Product priority, taste, budget, and privacy stay
with the operator, permanently.

## 5. The organ map — every organ already exists

This initiative builds a role, not a platform. Ground-in-reuse audit:

| Comaintainer organ | Existing subsystem it reuses |
|---|---|
| Judgment instrument | the grounding gate's architecture — claim extraction → per-claim forced-choice judges → calibrated verdicts — retargeted from answer text to landings. The claims are §4.3's table |
| Memory | the notes store; episodes are curated artifacts beside it, and `rationalize` is the curator's tool |
| Allocation table | the work atlas; claims become grants |
| Work orders | session frames + ATOS feature artifacts; the objective contract is already specified and enforced |
| Evaluation | the gym/golden pattern (`gym/next-edit/golden/` scaffold: harvest, validate, score, precision audit) with BENCH_LOOP's dev/holdout discipline and RUNBOOK §6 noise bands |
| Constitution drift | the drift toolchain, pointed at the charter |
| Promotion | DEFAULTS_LEDGER — each autonomy step is a row with a falsifiable flip condition and a review-by date |
| Runtime | scheduled sessions (`schedule` skill) + on-demand at landings; mesh identity later |

Net-new artifacts: the episode miner, the charter document, the gym
scorer, and a verdict log. Net-new stores: **zero** — episodes are
flat committed files under `gym/comaintainer/` like next-edit's; the
verdict log is operational JSONL under `~/.sovereign/comaintainer/`,
with curated disagreements promoted into the committed golden set
(runs local, baselines committed — the bench pattern).

## 6. Training

### 6.1 Episode schema

```
{ context,            # what was in front of the agent: diff, plan, question, frame
  candidate_action,   # what was proposed or done
  verdict,            # one of the six, §4.1
  basis,              # the citation: § / note id / ledger row / measurement
  outcome_evidence,   # what later proved the verdict right (or wrong)
  tier }
```

### 6.2 Tiers — recall may be loose; the label may not

The next-edit golden set's discipline (its precision note, 2026-08-05)
carries over verbatim:

- **Tier A — measurement-settled.** A later instrument proved who was
  right: the claim-search ladder (23% of rescues destroyed, rejected),
  GLiNER P2.1(a) (no speedup, rejected), the sep knob matrix (nothing
  separates, closed a 10-week ledger row), the next-edit phase-0
  overturn itself. Gates score on tier A only.
- **Tier B — operator-settled.** A recorded override or approval with
  no instrument behind it. Training breadth, never gating — because
  the history also records the operator's priors being overturned by
  measurement, and the role must learn the instrument outranks
  everyone's confidence, including the operator's. This is the
  anti-sycophancy design, structural rather than aspirational.
- **Tier C — inferred.** A `fix:` commit names the defect its feature
  commit introduced: reconstruct the pre-state, and the review episode
  has its answer key in the fix diff. 218 fix commits to harvest;
  semi-automatable; weakest labels, audited by sample like
  `AUDIT_SAMPLE.txt`.

### 6.3 Mining rules, per source

- 60 reject/revert/overturn commits → verdict episodes, directly.
- 29 attempts + 276 invariants → **tripwire negatives**: plant the
  violation in a synthetic diff; the candidate must flag it with the
  right basis. (§18.1 applied to the gym itself: a gate must be
  watched to fail — negative controls are built in from day one, the
  way `test(desktop): negative controls — prove the suite can fail`
  did it.)
- DEFAULTS_LEDGER rows → flip-condition authorship exercises: given
  the feature, write the falsifiable flip condition; diff against the
  ledger's.
- ~2,600 operator turns → intervention episodes: given the
  pre-context, would the operator intervene, and with what? Mined
  locally; **privacy-scrubbed before anything lands in-repo** (open
  question 3).
- 31 feedback memories → the rubric for the briefing-quality lane.

Honest expectation: 300–800 tier A/B episodes cold-start, plus
synthetic tripwires. Enough to resolve ~10-point agreement deltas
between charter iterations; deliberately thin for weight training —
the flywheel (§6.5) is what fills that, and pretending otherwise would
violate §18.5.

### 6.4 The gym

`gym/comaintainer/` mirrors `gym/next-edit/golden/`: harvest,
validate, score, precision audit. Scoring lanes follow the CI-gate
taxonomy:

- **HARD** — verdict agreement on the tier-A holdout; citation
  validity (mechanical: the anchor exists and bears on the case).
- **SOFT** — rationale and briefing quality, LLM-judged, tracked with
  a band, never gating (house policy on judge lanes).

Charter iteration runs on the dev split, promotion is judged on the
holdout, per BENCH_LOOP. The first run scores a **charter-less
frontier model** — the vibes-maintainer baseline every charter version
must beat, and the number that tells us whether the charter carries
signal at all.

### 6.5 The flywheel — the interface position is the training loop

From L1 on, every verdict logs `{episode, verdict, basis}`; every
operator override is a strong label; every verdict later contradicted
or confirmed by an instrument gets its `outcome_evidence` filled in
and its tier upgraded to A. A weekly distill pass proposes charter
amendments as reviewable diffs — the §18 clustering run, made
periodic.

Arithmetic, order-of: 20 orders/week × 3 verdicts ≈ 250/month; a 10%
override rate ≈ 25 strong labels/month compounding the golden set,
plus outcome backfill on everything else. **This is the answer to "how
do you train the role": position it where corrections are cheapest to
capture, then capture all of them.** Four months of history is the
cold start; the interface position is the data source.

### 6.6 The weight-space endgame

Decompose the judgment pass into **calibratable forced-choice
probes** — "does this diff plant a silent fallback?", "does the cited
lane match the changed subsystem?", "is there a test that can fail?" —
exactly the grounding gate's per-claim judge shape, calibrated
per-probe on a 1.5–4B local model, with a routing layer above. Not one
oracular judge. The ladder rejection (2026-08-05) is inherited as
architecture, not as a scar: it proved that two instruments with
different tau semantics disagree precisely on the cases that matter —
so **every probe ships with a shadow mode and an agreement measurement
against the calibrated reference before it gates anything.** The
system already owns the calibration machinery, the shadow-flag
pattern, and the housekeeping to keep the shadow honest.

Why weight-space at all: the comaintainer sees everything (privacy —
must be able to run fully local), runs at every landing (cost — cannot
be frontier-priced forever), and is the product's own thesis (a
sovereign model distilled from your corpus). It is the last phase, and
the gym built in P1 is what makes it a measured migration instead of a
leap.

## 7. Milestones — supervised from minute one, promoted as ledger rows

**(Amended 2026-08-06, operator redirect.)** The original ladder
started the role in unattended shadow; the operator redirected to the
self-driving shape: the role sits in the live loop from the start as a
**director** whose every directive passes a supervision moment, and
autonomy is granted per directive kind as the measured edit rate
earns it. Landing-shadow sweeps are demoted to optional secondary
data.

The director never sends a directive, work order, or review verdict
to a worker without first showing the operator a typed draft —
`{to, kind: order|steer|review|briefing, draft, reasoning,
citations}` — and receiving approve/edit/reject. The final (possibly
edited) version is what is sent; every (draft, final) pair appends to
`~/.sovereign/comaintainer/directives.jsonl` via
`scripts/co-directive-log.sh`. **The per-kind edit rate is the
disengagement metric.** Glassbox reasoning on every draft is what
makes the refinement loop work: an uncited draft is an unfinished
decision (§9).

Every promotion is a DEFAULTS_LEDGER row: falsifiable flip condition,
settling data, review-by date. Thresholds are set from M0 data, not
invented here (§18.4).

- **M0 (now).** Every directive supervised. Honest enforcement note:
  at M0 "never send unsupervised" is charter-enforced (remembered,
  not structural) — acceptable only because the operator is in the
  loop by construction.
- **M1.** Batch-approve / auto-send for directive kinds with
  sustained near-zero edit rate over a trailing window. From M1 on
  the send path goes through the helper with an explicit per-kind
  operator-ack flag (structural, §7).
- **M2.** Autonomous within bounded kinds; operator reviews async.
- **M3.** Default interface (the old L3): the operator talks to the
  comaintainer; the comaintainer talks to the pool. The operator
  retains direct access to everything — a default, not a wall.

## 8. What this deletes — the funding case

Plans fund only if they net-simplify. The deletes:

- **CLAUDE.md shrinks.** The per-session obligations that exist only
  because no role owns them — notes triggers, ledger enforcement,
  frame curation, drift cadence, coordination protocol detail —
  migrate into the charter. Ratchet: 55KB of per-session injection →
  target under 20KB, measured by `cache-audit` fleet-wide. Workers
  boot with the ten plus their order.
- **Frames get a curator.** 107 live frames → the genuinely in-flight
  (target under 20); the carried-items audit becomes a sweep instead
  of a per-frame plea.
- **The unstaffed instruments stop being everyone's job**, which is
  the same as stopping being no one's: rationalize, review-by,
  drift refresh, posture, fleet-report each land on the role's
  calendar.

Concept count: +1 role, +1 episode format. Stores: +0. Knobs: the
ladder replaces the standing ambiguity of "should I ask the operator."
The initiative is funded by the instructions it retires.

## 9. Risks, named

- **Bottleneck.** Judgment batches at landing points; mechanical gates
  stay parallel and code-owned; override is always one command. If
  the comaintainer is slower than the pool it serves, that is a
  finding, and L2 does not flip.
- **Goodhart on operator agreement.** Tier A outranks tier B by
  construction; citation validity is scored so the role cannot win by
  flattery; could-not-judge is a scored verdict and the gym plants
  unjudgeable episodes to keep it honest.
- **Charter ossification.** The drift toolchain covers the charter;
  amendments are weekly, diffed, and operator-approved.
- **Privacy.** Transcript mining runs local; anything committed
  in-repo passes a scrub gate the operator defines first.
- **Two-instrument drift.** Every probe that might ever gate ships
  with shadow mode and an agreement measurement — the ladder lesson,
  applied before scale, not after.

## 10. Plan — the deliverable, phase by phase

The comaintainer is not a service and not a framework. It is
**four artifacts, all inspectable files, plus one protocol**:

1. **The charter** — `gym/comaintainer/CHARTER.md`. The role as a
   file: verdict vocabulary, the judgment pass, citation rules. It is
   the comaintainer in the sense that weights are a model. Versioned
   beside the gym that scores it; amended only by PR.
2. **The gym** — `gym/comaintainer/{harvest_episodes.py,
   cases.jsonl.gz, score.py}`. One command scores any (model, charter)
   pair on the holdout — tier-A verdict agreement plus citation
   validity — and `--charter none` is the vibes-maintainer baseline.
3. **The seat** — `scripts/co-review.sh [ref]`. Assembles the landing
   bundle (diff, message, lint/test/census results, matched notes,
   ledger grep), makes one headless model call with the charter
   (`claude -p` now; calibrated local probes at P5), schema-validates
   a single typed verdict, appends it to
   `~/.sovereign/comaintainer/verdicts.jsonl`:

   ```json
   {"verdict": "measure-first", "instrument": "retrieval-prod",
    "basis": ["ARCH §18.5", "note 485f9f05"],
    "ask": "a single synth run is cited as the win; re-run n=3 or cite the lane"}
   ```

   `--override "reason"` lands anyway — and logging the override is
   what mints the training episode.
4. **The order** (as-built, 2026-08-06) —
   `.sovereign/features/<id>/order.md`, one file per order:
   objective with done-when / not-worth-continuing-if (the §2.1
   contract), lane, scope (including the shared-resource convention:
   daemon-touching orders also claim `~/.sovereign/config.toml`),
   budget, seams. `scripts/co-order.sh` (new/list/check/close) is
   convenience — the file is the truth, hand-editing is always valid,
   and `check` is advisory with nothing gating on it. The session-boot
   hook shows one line per open order (silent when none exist;
   `SOVEREIGN_NO_ORDERS=1` opts out entirely): orders are opt-in per
   session by construction — a session without one behaves exactly as
   before the artifact existed. Orders are gitignored per-host
   coordination, not PR ceremony.

5. **The director protocol + directives log** (as-built, 2026-08-06)
   — the M0 supervision moment (§7): the maintainer runs as a session
   booted on `CHARTER.md` plus the briefing pull; every directive is a
   typed draft the operator approves or edits before it is sent, and
   `scripts/co-directive-log.sh` records the (draft, final) pair —
   `--stats` computes the per-kind edit rate that flips M1. The seat
   (artifact 3, as-built `scripts/co-review.sh`) is the helper the
   director calls at landings; standalone shadow sweeps are optional
   secondary data. The seat keeps a stewardship log IN THE NOTES
   STORE (`related_entity: comaintainer-seat`; operator directive
   2026-08-06, revised same day from a flat oplog file — the store
   the seat curates is the store it logs to): machine logs record
   events, seat notes record why. Supersede/retire gives clean
   history instead of append-only; the anchor query is the day/week
   audit and the handoff — a successor seat holding it plus open
   orders plus the ledger takes over. Seat boot starts by querying
   it (MCP `notes` tool; the CLI can silently resolve a stray nested
   notes.db from a repo cwd — reflection filed 2026-08-06).

Everything else in this document is an operation on those artifacts.
The milestones (§7) only change **which directive kinds still require
the supervision moment**. Training (§6) only changes **which charter
and which engine the seat runs**.

Five phases. Each lands with its own Deletes line and its own note.

- **P1 — Mine (days).** Build artifact 2: `harvest_episodes.py` over
  git + notes + ledger (+ transcripts, local-only); tier labels;
  validation; committed golden v0 with a distribution report. **Done
  when:** ≥300 tiered episodes with a holdout split, and the
  charter-less baseline number exists.
- **P2 — Charter v1 (days).** Build artifact 1, distilled from
  episodes and the constitution — cited, not recalled. Iterate on dev
  per BENCH_LOOP; freeze against holdout. **Done when:** charter beats
  the charter-less baseline on tier-A agreement and citation validity
  by a margin stated before the run.
- **P3 — L0 shadow (1–2 weeks of ordinary work).** Build artifact 3
  and point the `schedule` skill at it: the seat sweeps each new
  commit on main; the log accrues; no one's flow changes; operator
  reviews a verdict sample. **Done when:** ≥50 shadow verdicts,
  agreement and noise floor measured, L1 flip condition written into
  the ledger from that data.
- **P4 — L1 advisory + flywheel.** One line added to the
  definition-of-done (run the seat, relay the verdict); override
  capture live; artifact 4 starts carrying new features; the weekly
  distill pass proposes CHARTER.md diffs as PRs.
- **P5 — Probe calibration.** Three probes (zero-test green, missing
  ledger row, doc-not-updated) calibrated on a local model, shadowing
  the frontier seat; agreement measured before anything gates. The
  gym is what proves the engine swap lossless.

## 11. Open questions for the operator

1. **Blocking point.** Is L2 (blocking) desired at all, or is
   advisory-forever the right fit for how you work?
2. **Runtime shape.** ANSWERED (operator, 2026-08-06): "I work with my
   comaintainer primarily and they spawn the sessions and provide
   oversight into them (glassbox style)." The director session is the
   operator's primary interface; workers are subagents the seat spawns
   after order approval (cap 3, standing rule), overseen live.
   As-built: `.claude/skills/comaintainer` — protocol over the four
   artifacts, no new infrastructure; daemon-native remains a P5
   question.
3. **Transcript privacy.** Which classes of transcript-derived
   episodes may be committed — paraphrase-only, pointer-only, or
   none?
4. **Curator authority.** May the comaintainer retire notes and
   frames autonomously from L1, or propose-only until L2?
5. **The name.** "Comaintainer" is used throughout; it will be typed
   a lot (CLI verb, ledger rows, gym dir). Last call before it
   ossifies.
