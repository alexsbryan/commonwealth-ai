# The Comaintainer Substrate — governing an agent fleet before slopageddon

**Date:** 2026-08-10
**Status:** design, pre-implementation. Every number below is either
measured in this repo on this date (the command is named) or cited to
the note that recorded it. Numbers that are **confounded** are marked
as such rather than quoted clean — §3 is the doc's methodological
spine and it applies to the doc's own evidence first.

← companions: `docs/COMAINTAINER.md` (the role this scales — read it
first; this doc assumes its §4 vocabulary and §10 artifacts),
`.claude/skills/comaintainer/SKILL.md` (the as-built M0 seat protocol),
`sovereign/ARCH_PRINCIPLES.md` (§18 is the judgment core; §19 is the
reuse obligation this doc answers in §9),
`sovereign/docs/WORK_ATLAS.md` (the coordination organ being
centralized), `quality/ARCH_LAYERS.toml` (the scheduler's input),
`gym/comaintainer/` (the charter, the gym, the measured baseline).

---

## 1. What is being prevented

**Slopageddon** is the per-company moment at which AI-generated code
volume overwhelms the organization's ability to judge it, and the
codebase stops being safely changeable. It is not "the code is bad."
It is a **rate crossover with an autocatalytic feedback loop**, which
is why it presents as a cliff rather than a slope.

### 1.1 The crossover

Generation scales as `devs × agents`. Review capacity scales as
`devs × hours`, and sublinearly — judging agent output is more tiring
than reading a colleague's, because the failure mode is plausible,
well-formed and wrong rather than obviously broken.

Order-of, at the target shape of 30 developers each running 3 agents,
assuming each developer sustains **half** this repo's measured solo
rate of 12.7 landed commits/day (`docs/COMAINTAINER.md` §1):

| Quantity | Value |
|---|---|
| Landings/day | ~190 |
| At 15 min for a genuine §18-grade review | **~47 human-hours/day** |
| Available, at 2h/day/dev of real review attention | 60 hours |
| Utilization | **~79%, with no slack** |

Every developer spends ~1.6h/day reviewing instead of building, and a
single bad week of estimates puts the org over 100%. The crossover is
not hypothetical at this scale — it is the default.

### 1.2 The autocatalysis

Past the crossover, review degrades to rubber-stamping, and
rubber-stamped landings accumulate exactly the failure class §18 was
distilled from 818 working notes to catch: **a plausible, well-formed,
exit-0 result that is wrong.**

The loop closes through *legibility*. Agents read the codebase to
decide what to write. Look at what this repo's eleven principles
actually constrain — one decider one name; closed sets are enums;
identity from essence, never a counter or an address; make it
structural, not remembered. Those are legibility invariants. Their
violations (the §15 smell table: two implementations of one threshold,
a `match` on string ids with more than 3 arms, an `Err` collapsed into
a success shape) each make the next agent's read of the codebase
worse, which makes the next agent's write worse.

**Slop is self-amplifying because it degrades the context its
successors are generated from.** That is the named mechanism behind
the cliff, and it is why the terminal state is worse than pre-agent
velocity: the org has more code, less legibility, and green tests that
mean nothing.

### 1.3 The three things that must be true to prevent it

1. Judgment capacity must scale with generation — which means a role,
   not more human hours (`docs/COMAINTAINER.md` §2).
2. That role's own error rate must be **measured**, or it is just slop
   with a verdict schema (§8).
3. The org must be able to detect the crossover **before** it
   happens — from a leading indicator, not from the wreckage (§4).

The comaintainer already answers 1 and 2. This document is about 3,
and about the substrate that lets one role serve thirty developers.

## 2. The journey at thirty developers

Per house practice, the design derives from the day. Three roles, each
scene stating what happens today, the one change the substrate makes,
and the technical fact it rests on.

### Scene A — the developer, mid-morning

**Today.** They hold three agents. Each returns "done" with a green
gate. They have no way to know whether the green is real without
reading the diff, and reading three diffs costs the hour they were
going to spend on their own work. The rational move — and the one
that produces slopageddon — is to skim and merge.

**With the substrate.** Their seat runs the §18 audit and returns one
typed verdict per landing with citations. The developer reads a
verdict line and approves, edits, or overrides. Editing is one action;
every edit is a strong label (`docs/COMAINTAINER.md` §6.5).

**Technical fact.** As-built and running: `scripts/co-review.sh`
assembles the landing bundle and returns a schema-validated verdict
(`~/.sovereign/comaintainer/verdicts.jsonl`, n=37 on this host).

### Scene B — the tech lead, deciding what lands first

**Today.** Ninety concurrent workers produce orders whose blast radii
intersect. The only serialization point is the merge queue, which
orders by arrival — so at 190 landings/day **the queue becomes the
bottleneck**, and conflict-driven rework is charged to whoever lost
the race.

**With the substrate.** Landing order is computed from blast
intersection and layer depth: non-intersecting orders land in
parallel; intersecting ones are ordered lowest-layer-first (§7).

**Technical fact.** Every input exists. `blast(symbol, max_depth)`
returns the transitive impact set and already carries a `concurrent`
field listing peer claims on that symbol. `quality/ARCH_LAYERS.toml`
is a **total** 20-layer map — every workspace member matches exactly
one layer — with two enforcement paths (Cargo-declared edges in <1s
via `xtask layer-gate`, SCIP-observed references via
`sovereign code arch-report`).

### Scene C — the CTO, at the quarterly

**Today.** They know throughput went up. They do not know whether
quality went down, because every available metric is confounded (§3).
They find out eighteen months later, when a routine change takes three
weeks.

**With the substrate.** One number per team: **planted-defect catch
rate**, trended, alongside edit rate. The pair distinguishes "my
people got good at this" from "my people stopped looking" (§4). That
is the slopageddon early-warning system, and it is the product.

**Technical fact.** The planted defects come from pre-labeled negative
space this repo already holds — 276 invariants (things that must never
be done) and 29 attempts (things tried and failed), per
`docs/COMAINTAINER.md` §3.

## 3. The obvious metric is a trap — measured here, honestly

Observational slop metrics computed from git history are the ones
every vendor will ship, because they are easy and they demo well. Run
on this repo today (`git log --since=2026-03-01`, plus a rework pass
counting file-touches that re-touch a file changed within 7 days):

| Month | Commits | `fix:` % | revert/reject % | rework ≤7d |
|---|---|---|---|---|
| 2026-04 | 540 | 11.7% | 0.0% | 65.6% |
| 2026-05 | 335 | 9.3% | 1.2% | 49.5% |
| 2026-06 | 276 | 9.4% | 0.7% | 54.2% |
| 2026-07 | 391 | **22.0%** | 0.5% | 49.0% |
| 2026-08 (partial) | 200 | 20.5% | 4.0% | **20.7%** |
| **Total** | **1,744** | **14.2%** | — | — |

Two headlines present themselves, and **neither is safe**:

- **"Fix ratio doubled in July and held."** True in the data,
  confounded in meaning. More slop, better `fix:` labeling discipline,
  or more shipped surface producing more findings — the ratio cannot
  distinguish them, and this repo's commit-message conventions
  demonstrably tightened over the same window.
- **"Rework collapsed in August, when the comaintainer went live on
  08-06."** Largely an **artifact of the instrument**. The pass walks
  `git log` newest-first, so a file's most recent touch can never be
  counted as rework; August, being the newest month, has a
  disproportionate share of such touches and is systematically
  undercounted. The honest April–July read is flat-to-mildly-declining
  with no clear signal.

**The rule this establishes, and the reason the section exists:**
observational slop metrics are confounded in *both* directions —
they move for reasons unrelated to slop, and they fail to move when
slop is accumulating. A dashboard built on them is worse than nothing,
because it manufactures confidence. This is §18.4 (validate the
instrument before the result) applied to the product's own headline
number.

The git-history diagnostic still has a role — as a **conversation
opener requiring zero adoption**, delivered with its confounds stated.
Stating them is the credibility move no competitor will make.

## 4. The real instrument — planted controls and the two-signal read

Slop cannot be measured observationally. It must be measured with
**controls**, continuously, from inside the review loop.

### 4.1 Mechanism

1. **Plant known defects** into the review stream, synthesized from
   the org's own pre-labeled negative space — here, 276 invariants and
   29 attempts. Each plant carries the basis it should be caught by,
   so a catch is scored on citation validity, not just detection.
2. **Score catch rate over time**, separately for the seat and for the
   human at the supervision moment.
3. **Read the two signals together.** This is the whole instrument:

| Edit rate | Planted-defect catch rate | Reading |
|---|---|---|
| falling | **held** | genuine competence — the autonomy ladder may advance |
| falling | **also falling** | **rubber-stamping — the slopageddon leading indicator** |
| held | falling | the charter is degrading relative to the work |
| held | held | steady state |

### 4.2 Why this closes a gap in the existing ladder

`docs/COMAINTAINER.md` §7 promotes M0→M1 on **edit rate alone**
("the per-kind edit rate is the disengagement metric"). Edit rate
alone cannot distinguish the first two rows of that table. An
organization whose reviewers stop reading promotes its seats to
autonomy *faster* than one whose reviewers stay engaged.

This is not a hypothetical defect. The first operating-data review
(note `e10b02a8`, 2026-08-10) records that the metric is currently
**unmeasurable** at all: `edited` is computed by string inequality,
the seat's summary-final convention logs 77 of 79 directives as
edited, at least 34 finals say approved-unedited, 5 are
operator-material edits, and 40 are indeterminate. The ladder's rung
condition cannot be computed today.

Two fixes, both required, and the first is a prerequisite for the
second being meaningful:

- **Make `edited` a recorded fact, not a string comparison.** A
  structured approve/edit surface — a button and a diff against the
  draft — makes edit class structural rather than inferred (principle
  10: make it structural, not remembered).
- **Add the catch-rate signal**, so a falling edit rate can be read.

### 4.3 Why only this position can measure it

Catch rate requires injecting a control into a real review and
observing a real reviewer's real response. That is available exactly
once per org, at the point where landings meet judgment. The
comaintainer occupies that point by construction.

`docs/COMAINTAINER.md` §6.5 calls the interface position "the training
loop." It is also **the measurement position**, and that is the more
defensible claim: training data can be bought or synthesized;
a controlled measurement of whether a specific organization's humans
are still looking cannot.

## 5. The substrate — five shared objects

At one operator these are files on one machine. At thirty developers
they are the substrate, and four of the five already exist.

| Object | As-built today | What thirty developers require |
|---|---|---|
| **Claim ledger** — who is touching what | `sovereign-work-atlas` crate, live across 4 mesh nodes; claims (TTL 4h default, 24h max) + CodeWatcher observations (`active` ≤300s, `recent` ≤1800s) | symbol granularity; leases, not advice; scoped queries |
| **Verdict log** | `~/.sovereign/comaintainer/verdicts.jsonl`, per-host, n=37 | shared — one seat's verdict is evidence for another's |
| **Charter** | `gym/comaintainer/CHARTER.md`, 87 lines, **sha256-stamped on every directive and verdict** | org constitution + team deltas; amendment by review board |
| **Backlog + ruler** | notes store (`related_entity=backlog`) + `quality/backlog-ruler.toml` v2, six axes, read-time ranking | one org heap, one versioned ruler |
| **Landing scheduler** | **does not exist** | the one net-new organ (§7) |

The charter hash deserves emphasis. Because `charter_sha256` is
stamped on every directive and every verdict, the system can already
answer *"which version of the engineering constitution judged this
landing?"* That is compliance-grade provenance, and it is unusual.

### 5.1 The charter as a portable artifact

Object 3 in the table above is the one with a property the other four
lack, and it is not yet being used: **the charter is content-addressed,
and it is already repo-independent.** Those two facts together make it
shareable, and a shareable charter is the bottom-up route to the
interchange format §12 of the business model needs.

#### 5.1.1 What is as-built

Verified at the call sites, 2026-08-11:

| Fact | Evidence |
|---|---|
| The charter is 87 lines, hand-versioned "v7", sha `eaa5c2ad…` | `gym/comaintainer/CHARTER.md` |
| Stamped on every verdict | `scripts/co-review.sh:174` |
| Stamped on every directive (both emit paths) | `scripts/co-directive-log.sh:329,367` |
| Stamped in every gym run's metadata | `gym/comaintainer/score.py:554` |
| **Zero repo-specific references** — no crate, path, or toolchain names; the content is decision rules plus calibration | `grep -cE 'sovereign\|commonwealth\|cargo\|svrn\|\.rs\b'` → **0** |

That last row is the load-bearing one. The usual thing that kills a
"share your standard" idea is that the standard is 80% facts about one
codebase. This charter is not: it is already the general artifact, and
nothing has to be extracted or generalized before it can travel.

#### 5.1.2 Two defects that block multi-charter work today

Both are small, both are on the path, and the second matters more than
its size suggests.

1. **Charter path resolution is implemented twice, differently.**
   `scripts/co-directive-log.sh:293` honors a `CO_CHARTER` environment
   override; `scripts/co-review.sh:146` hardcodes
   `(Path(gym)/"CHARTER.md")`. **A directive can be issued under an
   alternate charter, but a verdict cannot be rendered under one** — so
   the system cannot currently A/B its own charter end-to-end. Two
   implementations of one decision, smell §10.6.

2. **The stamp is not derived from the bytes that were used.**
   `co-review.sh:146` reads the charter text; `:174` re-reads the file
   to hash it. Milliseconds apart in practice, and harmless today — but
   a provenance hash whose input is a second read is not provenance, it
   is a coincidence that usually holds. If this hash is to be
   compliance-grade (§8), it must be computed from the bytes that
   actually entered the prompt.

#### 5.1.3 The property that makes a community possible

A sha256 is a content address, which buys four things at no cost:
provable identity ("same hash, same standard"), forkability (a fork is
a new hash plus a parent pointer), attribution (verdicts trace to the
charter that produced them), and — because verdicts carry outcomes —
**a charter accumulates a track record.**

The existing ecosystem of shared agent instructions (prompt
collections, `CLAUDE.md` gists, rules directories) is large and
low-signal for exactly one reason: there is no way to tell whether any
of it works. Adoption runs on the author's reputation.

A charter carrying a hash, a verdict log and a scored bank is a
different class of artifact:

> The unit of sharing stops being *"here is my standard"* and becomes
> *"here is my standard and its measured agreement rate."*

⬢ The measurement already exists — **56.9% tier-A agreement with the
charter vs 36.1% without, +20.8pt, p=0.0015, noise floor exactly
zero.** §8 lists that as evidence the charter carries signal. It is
also the product claim: teaching this thing demonstrably works, and
the number is the answer to a user's real question — *did my training
take?*

#### 5.1.4 Paired scoring, not a leaderboard

The obvious design — a public bank, a ranked list of charters — is
Goodhart bait, and it inherits every pathology of benchmark chasing.

The honest design exploits the zero noise floor instead. To evaluate
someone else's charter you run it **against your own episodes**,
paired with your current charter, and read which one agrees with *your*
judgment more:

> You do not adopt a charter because it is popular. You adopt it
> because it scored higher against your own judgment.

Four consequences worth stating:

- **No trusted registry is required.** Pull a charter, score it locally,
  keep it or discard it. Distribution can be a git URL.
- **The comparison is exact, not sampled** — two identical
  `--charter none` runs were byte-identical on all 90 holdout rows, so
  a paired delta on a fixed bank is a measurement rather than an
  estimate (§18.4).
- **The cost is bounded and known.** A paired evaluation is two scored
  runs at ~7,200 in / 700 out per episode; `score.py --rescore` then
  replays either side forever at zero model calls.
- **It composes with §10 prediction 6.** That prediction — does the
  default charter retain its advantage on a repo without a notes store —
  is not merely an onboarding question. **It is the precondition for
  charters being a shareable artifact at all.** If lift does not
  survive a repo change, there is nothing to share and this section is
  void.

#### 5.1.5 What has to change

Deliberately minimal. Three edits, no new subsystem:

| # | Change | Why |
|---|---|---|
| C1 | **One charter accessor**, honoring `CO_CHARTER`, used by both scripts and by `score.py` | Kills defect 1; makes end-to-end A/B possible. Principle 8 |
| C2 | **Hash the bytes that were read**, at the single site that reads them | Kills defect 2; the stamp becomes provenance rather than correlation |
| C3 | **A `parent_sha256` field** in the charter's own front matter | Lineage. A fork is then a DAG node, not a copy — and "which standard is this descended from" becomes answerable |

Not in scope, deliberately: a registry, a marketplace, ratings, hosting,
discovery. Those are distribution problems, and a git URL solves the
first version of all of them. **Build the artifact before the bazaar.**

Also to settle, and cheap now: `commonwealth-api/src/middleware/
context_injector.rs:341` already defines an unrelated `charter_hash`
for `project.toml` lifecycle. Two things named charter-hash with
different meanings is a principle-8 violation that will bite the moment
either becomes public. Rename one.

#### 5.1.6 The hard problem, named

Cross-repo comparability. My charter scored on my bank and yours scored
on yours are not comparable numbers. Two resolutions, and they imply
different infrastructure, so the choice should be deliberate:

| Model | Mechanism | Cost | Failure mode |
|---|---|---|---|
| **Paired-local** (recommended) | Each adopter scores candidates against their own episodes | Zero shared infrastructure; needs a local bank | No public claim is possible — "this charter is good" stays a private finding |
| **Shared bank** | One canonical episode set everyone scores against | A curated, maintained, versioned corpus | Overfitting to the bank; the bank becomes the standard instead of the charter |

Paired-local first. A shared bank can be added later as a *published
reference point*, never as the arbiter — and if it is ever added, its
own noise floor must be established before a single number from it is
quoted (§18.4).

#### 5.1.7 Phase placement and Deletes

Sits alongside **P1** (§11) — it touches the same scripts and blocks
nothing P1 needs. It is not a phase of its own; it is three edits.

**Deletes:** the second charter-path resolution in `co-review.sh` and
the second file read behind the hash. Two path resolutions become one
accessor, two reads become one. **Net concepts added: one**
(`parent_sha256`). **Net removed: two.** The ratchet holds.

#### 5.1.8 Predictions (extending §10)

| # | Prediction | Kill bar |
|---|---|---|
| 7 | With C1 landed, an end-to-end paired charter A/B runs on this repo and reproduces a lift in the direction of the ⬢ gym result | If the end-to-end lift contradicts the gym's, the gym bank is not representative of live directives and §8's headline claim needs re-scoping |
| 8 | An outside charter, scored paired-local against this repo's bank, moves tier-A agreement by a margin exceeding the zero noise floor in *either* direction | If every outside charter scores within noise, charters do not differentiate and the artifact has no community value — only the default matters |
| 9 | ≥10% of charter-authoring users publish or fork a charter within 90 days *(deferred until a free tier exists; stated now so it is not invented later)* | <2% means no bottom-up standard path; the interchange-format case reverts to needing a top-down standards play |

Prediction 8 is the cheap one and it is the real gate. It needs no
users, no registry and no free tier — only C1, a second charter, and one
paired run.

## 6. Central coordination — what it dissolves, what it costs

The mesh was the right substrate for a personal fleet across a handful
of workstations. For a single company it is the wrong one, and
centralizing the coordination plane dissolves three structural limits
outright.

### 6.1 What centralization removes

| Peer-to-peer limit (as-built) | Centralized |
|---|---|
| `sovereign-mesh::gossip::broadcast_now` reads the entry and POSTs to **every online peer** in parallel, fire-and-forget. Per-write cost is O(N); write rate also scales with N, so total traffic goes as **N²**. Four nodes → thirty is roughly 56×, atop a 10s anti-entropy round driven by a watcher that fires on every edit. | One writer-of-record; clients subscribe. The fan-out disappears. |
| Claims are **advisory** — no arbiter exists, so the boot banner warns into the void (this repo booted with 173+ files under concurrent peer edit and no arbitration). | Claims become **leases**. The plane can refuse. Admission control, not advice. |
| Observations are **file-granular**; `symbol_refs` is empty and `work_in_flight --match_mode=symbol` does not surface them at all (Phase 2b defers symbol granularity — it needs SCIP queries per changed file, per-node). | One SCIP index for the org. Symbol granularity becomes affordable once, not thirty times. |
| **One ambient session per workstation+repo, ever** (session segmentation is a deferred non-goal), so an observation cannot be attributed to which of a developer's three workers made it. | Workers register on spawn. Per-worker attribution falls out, and the seat's off-order safety switch becomes functional. |
| Landing order needs a global view of intersecting blast radii — expensive to establish under gossip. | Total order is trivial. The scheduler becomes buildable (§7). |

File granularity is the one that matters most and is easiest to
underestimate. At four nodes it already produced 173+ concurrently-
edited files. At thirty developers, file-granular collision detection
reports that everything collides with everything, thirty seats learn
to ignore it simultaneously, and the atlas joins the list of
instruments that "haven't stuck" — the exact failure
`docs/COMAINTAINER.md` §2 diagnoses.

### 6.2 The plane split — a day-one structural commitment

| Plane | Holds | Where |
|---|---|---|
| **Control** | orders, directives, verdicts, claims/leases, worker registry, escalation bus, charter hash | central |
| **Judgment** | needs the diff, history, gate artifacts, notes, ledger | **the tenant's compute** |
| **Presentation** | the pages; the structured approve/edit surface | central, edge-cached |

The control plane is small — four days of operation on this host
produced 145 directive records, 37 verdicts, and 25 orders. Kilobytes.
Sub-50ms reads are table stakes, not an achievement.

**The commitment: the control plane never holds diffs.** Made
structurally on day one (principle 10), that single constraint yields
three deployment postures from one build — metadata-only, managed
judgment, and fully self-hosted — and preserves the sovereign property
that made the mesh attractive in the first place. Made as a config
flag later, it yields one posture and a retrofit nobody funds.

### 6.3 What is genuinely lost

Offline operation, and mesh-native peer discovery for coordination.
The judgment plane still runs local, so a disconnected developer can
still be reviewed; they cannot coordinate. Against this target that is
the right trade, and it should be stated in the sales conversation
rather than discovered.

## 7. The landing scheduler — the one net-new organ

At ~190 landings/day, arrival-order serialization is the bottleneck.
The substrate's alternative:

1. Each order declares its scope; the seat computes the blast set
   (`blast(symbol, max_depth)`).
2. Orders whose blast sets **do not intersect** land in parallel —
   no queue, no serialization.
3. Orders whose blast sets **do** intersect are ordered by layer depth
   from `quality/ARCH_LAYERS.toml`: lower layers land first, because
   the map is a total dependency-direction contract and a lower-layer
   change invalidates upper-layer work but not vice versa.
4. Ties break on order age, then on the ruler's Value score.

Everything this consumes is already computed and already enforced.
The scheduler is arithmetic over existing surfaces, not a new
analysis — which is the §19 test for whether new capability is
justified.

**Open design question:** intersection is computed on declared scope,
which workers can drift from. The atlas observation stream is the
corrective, but only at symbol granularity (§6.1). The scheduler and
symbol-granular observations are therefore one piece of work, not two.

## 8. Who reviews the reviewer

If agents write the code and agents judge it, the judge is
slop-susceptible too. Most answers to this question are testimonials.
This one is a number.

| Claim | Evidence |
|---|---|
| The charter carries signal | **56.9% tier-A verdict agreement vs 36.1% charter-less, +20.8pt, p=0.0015** (`gym/comaintainer/`, note `e10b02a8`) |
| The instrument was validated before the result (§18.4) | **the noise floor is exactly zero** — two identical `--charter none` holdout runs produced byte-identical completions on all 90 rows, so deltas on this bank are exact rather than sampled |
| Every published number is re-checkable without spending a call | `score.py --rescore <run>` replays raw completions from `gym/comaintainer/runs/` — zero model calls |
| The judge cannot buy agreement with confidence | `could-not-judge` is a scored first-class verdict (§18.2's four verdicts); the gym plants unjudgeable episodes |
| The judge cannot win by flattery | citation validity is scored mechanically — the anchor must exist and bear on the case |
| Any landing is auditable to its constitution | `charter_sha256` on every directive and verdict |
| Honest failure is visible | 8 of 36 verdicts are `could-not-judge` (22%), every one from a malformed reply or an unreachable engine, **reported and never silently defaulted** (note `e10b02a8` FINDING 2) |

That 22% is the honest current state and is **not product quality**;
the fix is proven in-house (schema-forced decode, as `svrn backlog
add`'s scorer does it). Target <2%, stated as a kill bar in §10.

The `charter-less frontier model at 36.1%` figure is worth publishing
independently. It is a public baseline for "can a model judge agent
work," and the industry does not have one.

## 9. Reuse audit (principle 11 / §19)

| Substrate organ | Existing surface it reuses | Net-new? |
|---|---|---|
| Claim ledger / leases | `sovereign-work-atlas` (records, TTL, grades, privacy layers) | no — topology change |
| Verdict production | `scripts/co-review.sh` + `gym/comaintainer/{CHARTER.md,contract.txt,score.py}` | no |
| Supervision moment | `scripts/co-directive-log.sh` (draft/final pairs, pending→resolved latency) | no — needs a structured surface |
| Backlog + ranking | notes store + `quality/backlog-ruler.toml` + `scripts/co-backlog.py` | no |
| Scheduler inputs | `blast`, `quality/ARCH_LAYERS.toml` (20 layers, total, two enforcement paths) | no |
| Codebase legibility signal | `sovereign code fieldglass`, `arch_report`, the §15 smell table | no |
| Gym / scoring | `gym/comaintainer/{harvest_episodes.py,cases.jsonl.gz,score.py}` | no |
| Plant synthesis | `harvest_episodes.py`: `mine_tripwires()`, `parse_smell_table()`, `mine_attempts()`, `make_twins()` — **already minting planted defects**, 52 tripwire + 44 twin + 14 attempt episodes in a 303-case bank | no |
| **Plant rotation** | the bank is harvested once; no seed, no used-plant ledger | **yes (small)** |
| **Human-side catch measurement** | nothing measures whether the *operator* caught a plant | **yes** |
| **Central control plane** | replaces per-host JSONL + per-node atlas state | **yes** |
| **Landing scheduler** | arithmetic over existing surfaces | **yes** |

**Correction, 2026-08-10.** An earlier revision of this table claimed
the planted-defect harness did not exist. It does — `mine_tripwires`
has been minting invariant-negating proposals with answer keys since
the gym's first harvest, and a tripwire episode is precisely a planted
defect. That claim was made without checking the surface, which is the
§19 failure this repo has now recorded three times, each caught by the
operator rather than the builder. The corrected reading: **the model
half of the instrument is built and measured; only rotation and the
human half are new.**

Four net-new organs, one of them small. Everything else is a topology
or surface change to something already running.

### 9.1 What a window costs, measured

Local 36B engine (`FINAL-Bench_Darwin-36B-Opus-Q6_K`), timed from run
stamps against output mtimes on this host:

| Run | n | Wall clock | Per call |
|---|---|---|---|
| dev split | 211 | 9m46s – 18m22s | ~2.8–5.2s |
| holdout | 92 | 4m15s | ~2.8s |
| `--rescore <run>` on any prior run | — | seconds | **zero model calls** |

No API spend on the daemon engine; the frontier slice used 60 calls of
a 190 cap. The model-side lane costs ~10–18 minutes of resident-GPU
time per window and nothing to re-read.

The **human** side has the cost that matters: each injected plant is a
real review a person must perform. At ~10 plants/week/developer that
is 30–50 minutes of reviewer attention — spent taxing the exact
resource being measured. Statistical power comes from observations,
not observations-per-person, so at thirty developers **~1
plant/dev/week yields ~30 observations/week** org-wide. The instrument
is affordable precisely at the scale that needs it, and close to
unaffordable for a solo operator.

## 10. Falsifiable predictions and pre-registered kill bars

Stated before the runs, per §18.4/§18.5.

| # | Prediction | Kill bar |
|---|---|---|
| 1 | The planted-defect lane detects a catch-rate signal distinguishable from noise within 3 weeks of ordinary operation | If catch rate's noise band exceeds the effect it must detect (say ±15pt), the lane needs more plants per window before anything is built on it |
| 2 | Edit rate and catch rate **decouple** — i.e. they are not measuring the same thing | If they correlate above ~0.8, catch rate adds nothing and the ladder can keep promoting on edit rate alone |
| 3 | Making `edited` structural reduces indeterminate directive classifications from 40/79 to 0 | If any remain indeterminate, the surface is not actually recording the fact |
| 4 | Schema-forced decode moves `could-not-judge` from 22% to <2% without moving the tier-A agreement number | If agreement falls, the schema is coercing verdicts rather than formatting them — worse than the malformed replies |
| 5 | Blast-intersection scheduling raises parallel-landing throughput over arrival order, replayed against this repo's 1,744 commits | If the uplift is under ~20%, the merge queue was never the bottleneck and the scheduler is not worth its complexity |
| 6 | The default charter retains most of its advantage on a repo without a notes store | If lift falls below ~+8pt median across three repos of differing documentation density, "mine your history" is not the onboarding — a strong default charter with thin org deltas is |

Prediction 6 is the one that matters least at this target and most at
any other: with 30 developers the flywheel produces ~750 strong labels
per month (30 × the §6.5 solo arithmetic of ~250 verdicts and ~10%
override), which regenerates this repo's entire four-month cold-start
corpus (300–800 episodes) in about one month of operation, **with
nobody writing a note.** The organization bootstraps its own case law.
The default charter only has to be good enough to start.

## 11. Phases, each with its Deletes line

Plans fund only if they net-simplify. Each phase names what it
retires; where a phase adds more than it removes, it says so.

**Ordering correction (2026-08-10).** An earlier revision ran the
planted-defect lane first. It cannot: the lane's human half measures
whether a person caught a plant, which is unreadable while `edited` is
a string comparison reporting 40 of 79 directives as indeterminate
(note `e10b02a8` FINDING 1). The supervision surface is the
prerequisite, and the lane's model half is already built (§9), so the
cheap half can run alongside P1 for free.

- **P1 — The structured supervision surface.** Approve/edit as a
  recorded action; `edited` and edit class become facts rather than
  inferences. **Done when:** prediction 3 has a verdict — zero
  indeterminate classifications. **Deletes:** the string-inequality
  `edited` computation and the summary-final convention that made it
  meaningless. **Runs alongside, at ~15 min/window and no new code
  beyond a rotation seed:** the existing gym scored on a schedule, so
  the model-side catch-rate trend accrues while P1 lands.

- **P2 — The planted-defect lane, human half.** Plant rotation (seed +
  used-plant ledger over the 276-invariant pool, of which ~52 are in
  use); injection into the live review queue; catch rate scored for
  the person, not just the model; the two-signal read joined to
  edit-rate windows. **Done when:** a catch-rate figure with a stated
  noise band exists for both seat and operator, and predictions 1–2
  have verdicts. **Deletes:** the edit-rate-alone promotion criterion —
  a knob whose value could not be computed.

- **P3 — The central control plane.** Orders, directives, verdicts,
  leases, worker registry, escalation bus. Plane split enforced
  structurally. **Done when:** two developers' seats coordinate through
  it with no per-host state. **Deletes:** per-host `directives.jsonl` /
  `verdicts.jsonl`, gitignored per-host order files, launchd
  scheduling, the atlas's per-node broadcast fan-out, and the
  seat-death hole ("if the seat session is gone, escalations land
  nowhere" — SKILL.md), which demotes the TTL and park protocols from
  load-bearing to redundancy. **Ratchet:** stores +1 −4; knobs: the
  standing ambiguity of "should I ask the operator" is replaced by
  explicit per-kind authority.

- **P4 — Symbol-granular observation + the landing scheduler.** One
  org SCIP index; blast-intersection ordering. **Done when:**
  prediction 5 has a verdict from a replay over 1,744 commits.
  **Deletes:** arrival-order serialization as the only landing policy,
  and manual landing-order selection. **Honest note:** this phase adds
  more than it deletes. It is funded by P3's ratchet, not its own.

- **P5 — Org charter governance.** Constitution + team deltas,
  amendment by review board, autonomy granted org-level on pooled
  evidence rather than per-seat. **Deletes:** per-developer autonomy
  ladders, and with them the incentive inversion in §12.

## 12. Risks

- **Goodhart on the human, not the model.** A developer who
  rubber-stamps advances their seat's autonomy faster than a careful
  one. Solo this is self-defeating; across thirty people it is a race
  to the bottom. Mitigation is structural: autonomy granted
  org-level on pooled evidence (P5), and planted controls that make
  disengagement visible (P1). This is §18.1 — a gate you have not
  watched fail is not a gate — turned on the operator.
- **The instrument becomes the target.** Once catch rate is a
  reported number, plants will be recognized. Mitigation: plants are
  drawn from a rotating pool and synthesized per-window, and plant
  recognizability is itself sampled and audited (the
  `AUDIT_SAMPLE.txt` pattern).
- **Charter fragmentation.** Frontend and infrastructure teams have
  genuinely different standards. Unmanaged, the constitution forks
  thirty ways and the provenance guarantee is worthless. P5 is the
  answer and it is organizational as much as technical.
- **Escalation routing becomes org structure.** Today all escalations
  route to one operator. At thirty developers, some are the
  developer's, some the tech lead's, some the architect's. Getting
  this wrong reproduces the bottleneck the substrate exists to remove.
- **Superlinear failure.** The substrate's value compounds across
  seats and so does its noise. A coordination surface that is 70%
  false positives is not 70% useful — thirty seats learn to ignore it
  at once. Symbol granularity is a prerequisite, not an enhancement.
- **Confounded-metric temptation.** §3's table is easy to screenshot
  and hard to defend. Publishing it without its confounds would be the
  precise failure this product claims to prevent.

## 13. Open questions

1. **Authority model.** Who may grant an autonomy step — the
   developer, the tech lead, or a review board? P5 assumes a board;
   that is a guess about how engineering organizations actually work.
2. **Charter deltas.** Do team deltas *override* the org charter, or
   only *add* to it? Override permits drift; add-only permits
   contradiction. Neither is obviously right.
3. **Pooling consent.** Does cross-org episode pooling — the thing
   that would make the gym compound across customers — have a version
   an enterprise buyer will sign? If not, each org's flywheel is
   self-contained, which is slower but a cleaner story.
4. **Plant provenance.** Do planted defects land in real branches, or
   in a parallel review stream? Real branches measure the true
   pipeline and risk a plant escaping; a parallel stream is safe and
   measures a slightly different thing.
5. **Where judgment runs by default.** Metadata-only preserves the
   thesis and pushes GPU cost onto the customer; managed judgment is
   an easier sale and a harder security review.
