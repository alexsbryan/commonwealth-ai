# Comaintainer Operator Manual

The one-page quick reference for the operator's own hands. The seat
runs most of these for you during a session; everything here is safe
to run yourself, any time, in any terminal at the repo root. Nothing
in this file is agent protocol — that lives in
`.claude/skills/comaintainer/SKILL.md`; the role's design is
`docs/COMAINTAINER.md`; the ruler the backlog is scored against is
`quality/backlog-ruler.toml`.

CLI note: `svrn` is the prod symlink. Hosts that predate it carry the
legacy `sovereign` symlink instead — every `svrn` command below works
with `sovereign` substituted.

## The four pages — three that show, one that acts

| I want to see | Run | What renders |
|---|---|---|
| The morning glance — what changed in the architecture since I last looked | `svrn code fieldglass --open` | The fieldglass page (browser). Full render takes minutes (the duplication tier embeds); `--no-dup` is the quick pass. Evidence only, never verdicts — `docs/FIELDGLASS.md`. |
| Everything waiting on me | `scripts/co-closeout.py --open` | The closeout page: pending decisions first (each with its stated default if you say nothing), then resolved-in-window, open orders, recent verdicts. |
| The backlog, ranked | `scripts/co-backlog.py --open` | The heap as the ruler scores it today. Each card is headed by the item's NAME — `svrn backlog add` drafts it, a hand-written item falls back to its own first sentence — with the ref hash demoted to the metadata line under it. Unvetted items render greyed with the missing line named. Machine-scored items say who scored them. |

Rendered copies persist at `~/.sovereign/comaintainer/{closeout,backlog}.html`
and `~/.sovereign/arch/<corpus>/fieldglass.html` — re-openable without
re-rendering.

**Those three pages are read-only** — they show you what is waiting. The
fourth one acts.

## The console — the page that actuates

```
scripts/co-console.py              # serve, open a browser, Ctrl-C to stop
scripts/co-console.py --port 8731  # a fixed port instead of an ephemeral one
scripts/co-console.py --no-open    # print the URL, do not launch a browser
```

Four panes. **What do you want to do?** is an open box — say it plainly
and R0 reads the intent and proposes which roles do it, in order, with
each one's input already filled in. Press `/` to jump there, Ctrl+Enter
to submit. It **proposes and never dispatches**: you get runnable steps
you can edit, and nothing happens until you press run. If it cannot tell
what you mean, or you asked for something needing an id it was not given,
it says so and returns no steps — that is the answer, not a failure.

**Starting a campaign is two steps and R0 knows it**: R2 drafts the bars
the campaign is judged by, then R1 drafts the order that serves them. An
order written before the bars will not name what it moves.

**Waiting on you** is every pending directive with its draft
verbatim — `a` approves it as written, `e` opens it for editing
(Ctrl+Enter submits), `r` rejects it with a reason. `j`/`k` move between
cards. **Actuate** runs any of the six roles: press `1`-`6` to jump to a
role, type its input, run it. R4 takes 1-2 minutes and its canary doubles
that, so a run is a job the page polls — you get the real output in
place. **Standing** is open orders and recent verdicts, rendered by
`co-closeout.py`'s own functions rather than a second copy of them.

**It serves rather than exporting.** This is a foreground process that
lives as long as you are working and dies on Ctrl-C — not a daemon, and
nothing keeps running afterwards. A `file://` page cannot write to disk,
so the alternative was a Blob download or a clipboard round trip; and
actuation needs a round trip that export-then-apply cannot give you in
one sitting.

**The URL it prints contains a session token, and that token is the
credential.** Binding to 127.0.0.1 does not stop another page in your
browser from POSTing here, and these endpoints resolve directives and run
roles — so every request is token-checked and cross-site POSTs are
refused. Do not paste the URL anywhere.

**Nothing here is a second path.** Every action shells out through the
same argv `co-apply.py` builds, so a directive you approve on the page is
indistinguishable in `directives.jsonl` from one you resolved by hand.
Each action is also appended to `seat-actions.jsonl`, in the shape
`co-apply.py` consumes — so the log of a session is also a replayable
script.

**How a decision is recorded.** `approve` is `unedited`. `edit` is
`edited`/`content`. **`reject` is also `edited`/`content`** — you changed
the substance from "do this" to "do not", and `no-decision` means no
decision was taken on the row at all. The page says so on screen rather
than choosing quietly, because the edit rate is the statistic the M0 loop
is measured by.

`--self-test` runs the lane: token and cross-site refusals, the decision
mapping, and the job lifecycle. No socket left open, no store touched.

## Driving a role yourself

The six seat roles run on the local open-weight model, one card each in
`gym/comaintainer/roles/` (data — a card change needs no code change).

| I want | Run |
|---|---|
| Say what I want and be told which roles do it | `scripts/co-role.py R0 --input "start a campaign on X"` |
| Draft an order from typed intent | `scripts/co-role.py R1 --input intent.txt` |
| Draft campaign bars for an initiative | `scripts/co-role.py R2 --input initiative.txt` |
| See coverage against a campaign's bars | `scripts/co-role.py R3 --input financial-corpora` |
| Verify a landing | `scripts/co-role.py R4 --input bundle.txt` |
| File an out-of-scope finding as a backlog item | `scripts/co-role.py R5 --input finding.txt` |
| Propose retirements, bounded to named items | `scripts/co-role.py R6 --input "6f4928b8 45455f0e"` |
| Check every card parses and fits the budget | `scripts/co-role.py --lint` |
| Confirm R4's planted-defect canary can still fire | `scripts/co-role.py R4 --canary-only` |

`--input` takes a file path or a literal string. Each run appends one row
to `~/.sovereign/comaintainer/role-runs.jsonl`.

**What the gate class means for you.** `R0` is `propose` — it reads
intent and returns steps, queueing nothing, so asking it a question never
puts a row in the directive log. `R1`, `R2` and `R6` are `draft`:
they do not land anything, they queue a pending directive that shows up
in the closeout page for you to approve, edit or reject. `R3` and `R5`
are `auto` — consumer-validated, and a wrong item costs one heap row.
`R4` uses the charter's existing landing gate, unchanged; `--draft` and
`--auto` are refused on it, because that gate is ratified and is not a
command-line flag.

**Exit codes**, so a script can gate on it: `0` accepted or queued · `1`
the consumer rejected the output · `2` could-not-judge (engine
unreachable, malformed reply, an unbounded R6) · `3` engine drift — the
reply came from a model nobody pinned · `4` the canary halted the run.

**R6 must be bounded.** `--all` timed out at 900s over ~282 items, and a
sweep that times out reports nothing, which reads as "no dead items".

## Applying decisions in a batch

The console is the interactive path; this is the scripted one, and both
build the same commands. Use it to replay a `seat-actions.jsonl` the
console wrote, or to drive the seat from a script with no browser.

`scripts/co-apply.py <file.jsonl>` replays a list of decisions by driving
the real scripts — `co-directive-log.sh --resolve` with the explicit
`--edited`/`--unedited`/`--no-decision` flag you chose, `co-role.py` for
an actuation, `co-order.sh close` for a closure. Nothing writes to a
store directly, so a directive resolved this way is indistinguishable in
`directives.jsonl` from one you resolved by hand.

```
{"action":"resolve","id":"001b40b6","final":"Approved as drafted.","verdict":"unedited","edit_class":"none"}
{"action":"actuate","role":"R3","input":"financial-corpora"}
{"action":"close","order":"sec-filings-close","state":"landed"}
```

`--dry-run` prints what would run and touches nothing. A bad line is
reported with its line number and does not stop the others, but the exit
code is non-zero if any line failed. `--self-test` runs the lane.

An omitted or unrecognized `verdict` is REFUSED, never guessed — the edit
verdict is the statistic the M0 loop is measured by, and a fabricated one
would look exactly like a real one.

## The backlog

| I want to | Run |
|---|---|
| File a discovery (scored draft, you stay the vetter) | `svrn backlog add "<the discovery>" --objective "<what it serves>"` |
| File without a model score | add `--no-score` |
| Pull the top item as an order draft | `scripts/co-backlog.py --pull` |
| Check whether items still reproduce at HEAD | `scripts/co_liveness.py verify --all` (or name ids) |
| See what liveness is on record | `scripts/co_liveness.py ledger` |
| Check the backlog machinery itself | `scripts/co-backlog.py --self-test` |

A machine-scored item carries `Scored-by:` and cannot be pulled until
a person reviews it and clears that line — that review IS the vetting.
The same call also drafts the item's `Title:`, which is what you read on
the page; edit it like any other field if the model named it badly.

**Vetted has never meant still true.** Everything the vetting rule checks
is a property of the item, not of the code, so an item stays pullable
after the defect it names is fixed — measured 2026-08-12, three of the top
four vetted items were already closed. Every card now carries a liveness
line (`Never verified against HEAD` / `Verified alive 3d ago` / `STALE,
past the 14d window set in the ruler`), and `--pull` re-verifies the
handful of items it is about to hand you, in that moment, stating the
result in the draft. Staleness never blocks a pull, and you never have to
run `verify --all` first: skipping it for a month costs one run to
recover, because the only question asked is about HEAD. A `dead` verdict
is a proposal — the item stays on the heap saying so, and you retire it.
The full map is `scripts/BACKLOG.md`.

## The pool and the record

| I want to see | Run |
|---|---|
| Open orders — local files PLUS orders opened by a seat on any mesh machine, each with node attribution | `scripts/co-order.sh list` |
| One order's file | `.sovereign/features/<id>/order.md` (plain markdown, hand-editable) |
| Session frames (what each terminal's last session banked) | `svrn session frames` |
| Who is touching what right now (mesh-wide) | `svrn tools call work_in_flight --scope= --match_mode=file` |
| Is a SHARED resource (daemon, soak, snapshot) taken right now? | `svrn claim may-i daemon:<node>:<action>` — verdict `held` / `expired` / `free`, one call, no sockets |
| Take a shared resource for an operation | `svrn claim take daemon:<node>:<action>` (30-min TTL; `--ttl` to shorten), `svrn claim release <id>` when done |
| Which peer is my daemon serving right now? | `curl 127.0.0.1:9741/status` → `inference.peer_requests[]` (`active`, `served_total`, `name`) |
| The seat's edit-rate scoreboard — the MESH-WIDE number: the notes store is the denominator, so either seat reads the same count | `scripts/co-directive-log.sh --stats` |
| Directives carrying no edit verdict yet | `scripts/co-directive-log.sh --unclassified` |
| Recent landing verdicts | `tail ~/.sovereign/comaintainer/verdicts.jsonl` |

The scoreboard's `n` counts only rows where the seat STATED the verdict
(`--edited` / `--unedited` at resolve time, or a retrospective
annotation); `indet` / `nodec` / `unclass` sit outside that denominator
in their own columns so it is never quietly widened. Before 2026-08-10
the verdict was inferred from whether the final text differed from the
draft, which measured the seat's summary-writing habit and reported
97.5% edited against a true 13.3%.

Since 2026-08-11 (order seat-durable-rail) the tally is MESH-WIDE: the
denominator is the notes store — every directive the seat logs on ANY
machine writes through as a note anchored `directive-log` — so the
number reads the same from either seat, and the rows carry node
attribution. When the notes daemon is down, `--stats` falls back to
the local files and says so on its banner; the local number is not the
mesh number. Local rows that predate the write-through are excluded
from the mesh denominator and counted in that banner.

The raw machine logs live in `~/.sovereign/comaintainer/`:
`directives.jsonl` (every draft/final pair, plus
`directive-edit-verdicts.jsonl`, the append-only sidecar carrying the
edit verdict for rows logged before the flag existed), `verdicts.jsonl`
(every landing review, interactive and nightly).

## What accumulates, and what closes it

Every system below has a rich way to CREATE an entry. Not all of them
have anything that reads the signal that an entry is finished. A system
with a writer and no reader fills up with things that were true when
written and are false a week later, while still presenting as
authoritative — which is how three of the top four vetted backlog items
came to be already-fixed work (seat finding `14e2bcb3`, 2026-08-12).

**Read the last column first.** *Flat* means one run recovers from any
gap: the question asked depends only on current state, so a month of
skipped runs costs the same as a day. *Growing* means the catch-up cost
rises while you sleep — and a system whose catch-up cost grows gets
skipped once, then dreaded, then abandoned. Any `growing` row is a
future abandoned system, named here as such.

| System | Created by | Closure trigger | Who reads the trigger | An entry nobody re-checked shows | Recovery after a missed run |
|---|---|---|---|---|---|
| **Backlog** (`related_entity=backlog` notes) | `svrn backlog add`, `co-backlog-producer.sh` | "Does it still reproduce at HEAD?" | `co_liveness.py verify`; `co-backlog.py --pull` re-verifies what it hands out | `Never verified against HEAD` on the card; a stale one shows its age | **Flat** — verification is level-triggered and bounded by what is being pulled (proved: `--self-test`'s resilience battery, 30d and 300d gaps cost identically) |
| **Nightly sweep** (`co-sweep.sh`) | every commit that lands | the high-water mark at `~/.sovereign/comaintainer/sweep.last` | the next sweep — if it gets that far | nothing; the commit is simply never reviewed | **GROWING — this row is the warning.** `co-sweep.sh:21` keeps a mark, `:25` caps at 20/night, and ~20 commits/day land. Deferrals 2026-08-07..12: 4, 54, 52, 42, 36, 41. It has not caught up in six nights and structurally cannot. |
| **`DEFAULTS_LEDGER.md`** | a row per default-off / dark ship | the row's own `review-by` date | **nobody** | nothing — the date passes in silence | **No trigger to miss.** Self-documented at `DEFAULTS_LEDGER.md:1069`: "Nothing parses this file's review-by dates", on a row that "was FALSE for thirteen days and that is the lesson." |
| **Durable notes** (decision/invariant/todo) | `note`, `svrn notes add` | retire-with-pointer, or supersede | `svrn notes rationalize` — a candidate report, and only on request | the note, indistinguishable from a current one | **Flat**, but manual: `rationalize` derives candidates from the live store at read. Ephemeral kinds have a real closer (`svrn notes gc`, daemon-run daily); durable ones do not. |
| **Directives** (`directives.jsonl`) | every seat draft/final pair | the seat stating the edit verdict at resolve time | `co-directive-log.sh --unclassified` / `--stats` | listed under `--unclassified`, outside the denominator | **Flat** — both are queries over the whole log, computed at read |
| **Orders** (`.sovereign/features/<id>/order.md`) | `co-order.sh new` | `co-order.sh close` | `co-order.sh list` | listed as open, with its age | **Flat** — `list` derives from the order files that exist now |
| **Campaign bars** (`quality/campaigns/<id>.toml`; `initiative-bars.toml` is ARCHIVED 2026-08-17, frozen) | transcribed from a spec's own gate lines when the campaign starts; <=9 bars, one screen | a machine-stamped measurement row in `~/.sovereign/comaintainer/bar-measurements.jsonl` (`co-lineage.py measure`, nightly via co-sweep) — hand-written transitions are gone; defer/descope is a one-line `status` edit with git as the ledger; campaign close moves the file to `quality/campaigns/closed/` | `co-lineage.py coverage <campaign>` — headline is **uncovered bars**; stale/unattributed/static rows render loudly | `never-attempted`, counted OPEN, with `NO ORDER`, `LANDED-BUT-UNMOVED`, `NEVER-MEASURED` or `STALE` beside it | **Flat** — coverage and verdict are derived from the campaign files, the measurement store and the live order files at read; age renders from row `ts` |
| **Scope claims** | `declare_scope` | `release_scope`, or the TTL | `work_in_flight` | nothing: the TTL drops it whether or not anyone looks | **Flat** — live-only by design; the spec forbids history, so there is no backlog of claims to replay |
| **Session frames** | a session banking at its cutoff | a session marking itself `completed` | `svrn session frames` | `in-flight` forever, with its age beside it (87 live, several `in-flight` for days) | **Flat** — the list is derived from live frames at read |
| **Bench baselines / posture rows** | a bench run; each quality subsystem's artifact | re-running the named refresh command | `svrn posture` — one row each, each naming its own refresh command | `stale`, with its age and the command that fixes it | **Flat** — age is computed from the artifact on disk at read time |

Two things to take from the table. First, `DEFAULTS_LEDGER` is the one
row with no reader at all, and it is the same defect the backlog had
until this loop landed — a declared closure signal that nothing parses.
Second, the sweep is the one `growing` row, and that is exactly why the
backlog's loop does not depend on it: the sweep additionally proposes
closure candidates when it runs (an accelerator), but the heap is
correct while the sweep is behind, uninstalled, or permanently off.

## Initiatives — the bars, and which of them nobody is working on

An order has a falsifiable objective, a lane, a landing verdict and a
status. An initiative had none of that. Sixteen orders ran under
`NATIVE_GROUNDING.md`; every one passed its own bar; the initiative's
headline objective — parity at **≥5x lower gated-turn latency** — was
carried by none of them, and one of the spec's five mechanisms (H3) was
never ordered, never killed, never scoped. Both were found by hand.

**A parent pointer would not have caught either.** Had all sixteen
orders carried `serves: native-grounding`, the tree would have rendered
*healthier*: one spec, sixteen children, all closed, gates green.
Performed work was never the problem. So the view renders **bars, not
orders**, and its headline is the inverse: the bars with no order at all.

| Question | Run |
|---|---|
| What has this campaign promised, and what is uncovered / unmeasured / stale? | `scripts/co-lineage.py coverage <campaign>` |
| What actually happened — every measurement row, did the bars move? | `scripts/co-lineage.py postmortem <campaign>` |
| What campaigns exist / how many are closed? | `scripts/co-lineage.py list` |
| Measure every instrumented bar right now | `scripts/co-lineage.py measure <campaign>` (or `--all-active`) |
| Is the renderer itself honest? | `scripts/co-lineage.py --self-test` |
| Is a commit's story entailed by its diff? (shadow) | `scripts/co-drift.py <sha>`; self-tests: `--self-test`, `--self-test-live` |

### Floor, target, and yellow (2026-08-16)

A bar written before any code is a hypothesis about a threshold. With one
number, 88-against-90 is `failed`, `failed` reads terminal, and the worker
stalls. So bars carry two, with an asymmetry: **`floor` must be
data-backed** (a baseline, the incumbent path, or a structural zero —
named in `floor_basis`, which the loader requires), **`target` may be
invented**. No floor means target-only: red/green, and a near-miss there
genuinely escalates — the right shape for a structural zero.

`met-floor` is the band: measured, above floor, below target. **The work
ships; the bar does not close.** The decider computes it (only a
measured `met` or a `descoped` status edit closes a bar), so yellow
stays OPEN everywhere; the tuning debt is filed to the backlog heap
keyed by bar id (`measure` prints the ready-to-run producer line on
every `met-floor` row), and the heap's OVERDUE rendering carries the
review pressure.

**A worker may pass yellow; only you may move a target** (§18.6). That is
the whole reason it is safe to grant.

### The burden rule (operator, 2026-08-17 — a standing test, not a preference)

> "we need to absolutely prioritize telemetry that offers as much signal
> with as little burden as possible otherwise it will just be another
> speedbump we route around."

Every future bar or instrument must pass three questions before it ships:

1. Does anything have to be REMEMBERED to run? If yes, it will not get run.
2. Does it add a step to a path someone already walks?
3. Does the measurement cost more than the decision it informs?

Corollary, demonstrated the day this shipped: **the honest instrument
turned out to be the cheap one** — read-tier F2 (below) is both more
attributable and ~1000x cheaper than the nightly run it replaced. And a
coverage target needs a denominator someone CHOSE: 503 is a number a
filer's agent chose; asked-concepts is a denominator the user chooses by
asking.

### Why the 2026-08-17 financial-corpora thresholds are shaped this way (directive 0c042797)

Recorded so a later reader does not "fix" these back:

- **F2 is target-only 1.0 with NO floor, deliberately.** The bar is zero
  unattributable numerals, not a rate; a floor on an honesty bar licenses
  occasional fabrication. Measured n=3 was 1.0 / 0.889 / 1.0 — the 0.889
  is intermittency worth seeing, and a floor would have smoothed it into a
  yellow. Do not add a floor here "to make it reachable".
- **F2 is READ-TIER on purpose, and its number is allowed to go stale.**
  It parses the newest COMMITTED frozen-set verdicts (aggregating every
  artifact of that commit — an n=3 session counts all 27 items) and takes
  `ref` from that commit, so a value is never stamped with a HEAD that did
  not produce it. The run-tier alternative (regenerate answers nightly)
  was rejected at 40-70 min/night: staleness rendered as age is better
  information than a number kept fresh at an hour a night, and a slow
  path is the thing most likely to get the whole system abandoned. The
  read path is measured at 0.06-0.13s and is structurally
  daemon-independent (proved with sockets disarmed in-process).
- **F5 is NOT a coverage ratio.** 24 of 503 filer tags are mapped, but of
  the 479 unmapped, 335 have names over 34 characters (footnote minutiae
  like `AccumulatedOtherComprehensiveIncomeLoss…EffectNetOfTax`), only 5
  are variants of already-covered concepts, and the denominator contains
  dimensional/segment tags companyfacts can never serve — so any
  percentage target on it is partly unattainable by construction. F5
  lands as its first clause only: the corpus STATES what it cannot answer
  (structural, target 1.0). The real number is deferred until demand
  exists: `fixable misses / total asks`, trending to zero, with
  `consolidated_only` misses EXCLUDED from the denominator — a source
  limit to disclose, never a gap to close (miss instrumentation folded
  into order sec-filings-ship).

## Campaigns — approve the ladder once, not every rung

`scripts/co-campaign.sh new|list|check|close` — one gitignored file,
`.sovereign/features/<id>/campaign.md`, same contract as an order.

Per-order intake made a six-rung spec cost six interviews, five of them
restating the spec. You approve instead: the **ladder** (rungs in landing
order with their bar ids), the **ambiguity policy** (which principle
decides each axis the spec leaves open), the **tuning** bounds (cap,
dev/holdout split, knob whitelist), the **stop conditions**.

The seat then drafts, spawns, steers and lands against that one approval.
Four things still reach you: the premise is falsified, a bar needs
re-registering or a target moved, a commons/irreversible action, taste.
Findings outside a rung's Scope are **banked** — one item per key,
triaged at close-out. Calls made under the ambiguity policy append dated,
principle-citing lines to Decisions; that is the close-out read, with
`co-lineage.py postmortem <initiative>` beside it.

| Question | Run |
|---|---|
| Draft a campaign for a spec | `scripts/co-campaign.sh new <id> <title>` |
| What am I declining to pre-authorize? | `scripts/co-campaign.sh check <id>` — UNSET means that call escalates |
| What campaigns are open? | `scripts/co-campaign.sh list` |

Reading the output:

- **`UNCOVERED BARS = N` is the headline**, not orders closed. A bar no
  order names cannot have been met by accident.
- **Four verdicts, not two** (ARCH §18.2): `met` / `failed` /
  `could-not-judge` / `never-attempted`. `never-attempted` means no
  evidence event was ever recorded against the bar — not that it passed
  quietly — and it **counts as OPEN**. That rule is the same one
  `co-mesh-drill.sh f-assemble` learned the hard way: a headline that
  counts only what ran reports a clean number for a program that ran
  nothing.
- **`deferred` is not a verdict.** A bar moved out of a plan has not
  been judged, so it stays `never-attempted` *and* stays open — with
  the artifact that deferred it named on the row. This is the mechanism
  that hid the latency bar: it was never *failed*, it was re-scoped in a
  planning document.
- **`LANDED-BUT-UNMOVED`** — a bar whose covering orders all closed
  `landed` while nothing was ever recorded against the bar. Coverage
  alone cannot show this; verdict alone cannot show this. It is the row
  to read first in any post-mortem.
- **`(unattributed)`** on an order is legal and stays visible in the
  count. At minting, 16 of this tree's 39 orders carry `serves:` and 23
  do not; that is data about the process, not a defect to hide.

Adding an initiative is a transcription job, not an authoring one: its
bars come from the spec's own gate lines (`§7.3`-style "Beat / Kill"
clauses), each with the section cited in `derives_from`. If a spec
declares no falsifiable bars, the honest entry is the initiative with
zero bars and a `notes` line saying so.

## Health, gates, quality

| Question | Run |
|---|---|
| Is any quality subsystem stale? | `svrn posture` (each row names its refresh command) |
| Does the workspace compile? | `./scripts/sovereign-lint.sh --human --full` |
| Do tests pass? | `./scripts/sovereign-test.sh --human` |
| Did quality regress (retrieval/routing/synthesis)? | `./scripts/sovereign-ci-bench.sh --quick` (~35-40m; read lane KIND before the number) |
| Is the daemon healthy? | `svrn doctor` |
| How well did injected notes land and get used? | `svrn notes retrieval-audit` (hit-rate of hook-injected notes against the session transcript) |

## Seat-internal (listed so nothing is mysterious — you rarely run these)

- `scripts/co-review.sh <ref>` — the landing-review helper behind every
  verdict draft.
- `scripts/co-directive-log.sh` (write forms) — how the seat logs
  draft/final pairs; you only ever see the drafts themselves.
- `scripts/co-field.py` — the seat's fieldglass reader: changed-file
  evidence and landing diffs for verdict records (`docs/FIELD_VERDICTS.md`).
- `scripts/co-sweep.sh` — the nightly shadow sweep (launchd, 03:30):
  reviews every commit since the last sweep into `verdicts.jsonl`.
  `--install` / `--uninstall` manage the launchd agent.
- `scripts/co-backlog-producer.sh` — the contract automated signals
  file through (e.g. a failed HARD bench lane); never run by hand.
- `scripts/co-lineage.py` — the campaign rollup (see "Initiatives"
  above). Reads `quality/campaigns/*.toml`, the measurement store and
  every order's `serves:` frontmatter; `coverage` writes nothing —
  only `measure` appends rows (and their logs under
  `~/.sovereign/comaintainer/measure/`).
- `scripts/co-drift.py` — shadow drift monitor: per swept commit,
  claim-vs-diff and touched-files-vs-`serves:` rows into
  `verdicts.jsonl` (kind `drift`). Always shadow; a model never
  authors a rendered line.
- `scripts/co-order.sh new|check|close` — order lifecycle; the seat
  drives these, but the order file is always yours to edit directly.
  `new` now writes a `serves:` line (default `(unattributed)`) and
  `check` validates it against the declared bars — an unknown bar id is
  a NOT-READY problem, an absent one is a nudge.
  Since 2026-08-11 the mesh write-through is on by default and silent:
  `new` also opens a notes-store shadow (so any mesh seat lists it),
  `close` retires it — a daemon that is down is a named notice, never
  a failure.
- `scripts/co-mesh-drill.sh` — the two-seat conformance drill for the
  mesh-visible rail (UC-D1..D4 AND UC-F1..F8, four-verdict readings;
  procedure `scripts/CO_MESH_DRILL.md`). The seat runs it across the
  two machines when the rail changes. The D-cases need the operator to
  relay steps between machines; the F-cases run THEMSELVES — one
  hand-written start note, each side's `f-exec` on the epoch schedule,
  verdicts written as anchored notes, `f-assemble` builds the table
  from the notes alone (UC-F8; the operator's only act is the start
  note, and the deploy is seat-owned).
- `sovereign seat watch [--once]` — the notes-rail poller as a
  mechanism: surfaces new seat-addressed records (anchored
  `comaintainer-seat`, `order-seat`, `directive-log`) as `SEAT_WATCH`
  lines. This is the mechanism the F-drill runs from.
- Seat detection is ambient and skill-based (order commons-fluency,
  item 10): a session is in the seat when its transcript carries the
  comaintainer skill invocation — the ambient hook (inject-notes.py)
  scans the transcript on each prompt and then includes the
  coordination rail instead of withholding it (and the one-line
  withheld notice disappears from the prompt). No env is needed to
  boot the seat; ordinary sessions (no skill marker) are withheld as
  before.
- The seat boot block (order seat-boot-block): a seat session's FIRST
  prompt also carries one pre-assembled rail block
  (`## Seat boot block — the rail, indexed once`) — seat-anchor todos
  first, then recent seat decisions, open orders (`co-order.sh list`),
  directive-log stats — at a fixed ~3k-token budget, once per session
  (`boot-block.json` marker; a failed run writes no marker, so the
  next prompt retries). If the block is missing (daemon down at boot),
  `scripts/co-boot-block.sh` renders the same block on demand and
  writes the marker itself. Every injection — boot block and index —
  logs one row to `~/.svrnmesh/retrieval-log/<session>.jsonl`, which
  `svrn notes retrieval-audit` scores against the transcript.
- `SOVEREIGN_SEAT=1` — explicit one-off override only (back-compat):
  forces the seat read path for a single read without running the
  skill. Never required.

## Escape hatches (standing, operator direction 2026-08-06)

Everything is skippable. You can hand-run a worker in your own
terminal against an order file, ignore the briefing, or drop to plain
sessions any day — orders and the seat never make the simple path
harder. If a page looks wrong, the page's footer names the store it
read; the stores are the truth, the pages are views.
