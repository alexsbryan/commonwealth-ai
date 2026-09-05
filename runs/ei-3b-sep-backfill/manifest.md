# Run manifest: ei-3b-sep-backfill

- **Order:** `.sovereign/features/ei-3b-sep-backfill/order.md`, approved by the
  epistemic-index campaign ladder. Carved from ei-3-index under its park clause
  after the fourth daemon OOM of 2026-09-04. Serves EI4-self-describing-atlas,
  EI2-sep-unchanged.
- **Runs ONLY in a quiet window the seat announces.** A battery: no worker
  builds, no synth lane, no judge calls for its duration.
- **Staged at:** `8f3c129fa`, worktree `/home/alexbryan/dev/ei3b-wt`, branch `ei-3b`.

## What runs

`runs/ei-3b-sep-backfill/run.sh` wraps `sovereign/bench/sep_atlas/backfill_index.sh`.
The driver is the engine and is unchanged in shape — worklist, the two existing
verbs (`atlas migrate-all`, `atlas backfill-ann`), the JSONL ledger and resume
are all its (ARCH §19). The wrapper adds two refusals, per-leg and per-batch
markers, a `free -g` sample per batch, a terminal `DONE`, and the order's OOM
stop condition.

Backfills `atoms_ann.lance` across the 1,770 per-article `sep-<slug>` atlases.
Nothing else in a `sep-<slug>` atlas changes. The bare `sep` chunk index is a
control and is not touched, nor are `wessex-hoard` or
`brothers-karamazov-book-1`.

## Two legs

A 2.5 h battery whose first invocation is also its first test is a bad bet.

| Leg | What | Cost |
|---|---|---|
| 1 | `backfill_index.sh --limit 5` — proves dispatcher → exec → daemon probe → embed → ledger → marker → resume | ~35 s |
| 2 | the full sweep; the driver's own resume skips leg 1's five | ~4 h |

Leg 1 exiting non-zero means leg 2 does **not** start (`state: smoke-failed`).
`SMOKE_ONLY=1` stops after leg 1 deliberately.

## Precondition: THE BINARY (the open item this staging closes)

The previous staging pointed at a binary built **2026-09-04 15:11**. That
predates `c0f632403` (ei-3c: the seed table's population became map-derived —
the whole point of the table), `57e5ee76d` (ei-7c: the installed atlas serves
from the v2 store) and `78308e392` (ei-7a: `AtomType::Summary`). A 4 h battery
run against a binary that cannot see the change it exists to propagate is the
exit-0-and-wrong failure in its purest form.

So the check is **structural, not remembered** (ARCH §10, §7): `run.sh` refuses
to start when `$SCLI` — or the `sovereign-cli-llm` sibling the dispatcher execs
for `atlas` — is older than **the newest commit that touches what the binary is
built from** (`git log -1 -- '*.rs' '*Cargo.toml' 'Cargo.lock'`). Watched
failing 2026-09-05 against exactly the stale binary:

```
REFUSING: sovereign-cli built 2026-09-04T15:11:40-07:00, BEFORE the newest
source commit 7f60a09bd (2026-09-05T04:27:54-07:00) — rebuild: cargo build -p
sovereign-cli -p sovereign-cli-llm --features corpus-engine/treesitter,\
sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness
```

**The first version of this guard keyed on HEAD's date and was wrong**, and it
refused the seat's first launch attempt: the staging commit touched only shell
and markdown, so the binary was current and the guard said stale. That is the
guard working (it refused rather than ran) and the rule being wrong. The
reference is the newest *source* commit precisely so a docs- or
scaffolding-only commit cannot invent staleness, while a Rust change still
does. Both directions are watched: the current binary passes, the
2026-09-04 15:11 binary still refuses.

The binary this run uses is built by that exact command and is recorded, with
its mtime, in `DONE`.

*(Cosmetic, not a fault: the dispatcher prints "sovereign-cli-llm binary is 3s
older than the `sovereign` dispatcher". Both come out of one cargo invocation
seconds apart; `sibling::warn_if_stale`'s tolerance is 2 s. It is left
unsilenced — muting a staleness warning inside the script whose whole subject
is staleness would be the wrong instinct.)*

## Step 0 and step 0b, re-verified on THIS binary

Step 0 (`520aa6db8`) took the `ChatSession` out of `backfill-ann`; step 0b
(`20b1042da`, confirmed an ancestor of HEAD) took it out of `migrate-all`.
Both hold. Peak RSS from `/proc/<pid>/status` `VmHWM` — the same semantic as
`/usr/bin/time -v`'s maximum RSS, which is unavailable because **GNU time is
not installed in the `sovereign-vulkan` toolbox**. `bc` is not installed
either; `measure_rss.sh` does its arithmetic in bash integer nanoseconds
rather than printing a plausible `wall_s=0.00` over a missing tool
(ARCH §18.3).

`sep-18thGerman-preKant`, the same atlas step 0's own after-row used, n=3:

| binary | peak RSS (kB) | wall (s) | resolved |
|---|---|---|---|
| `520aa6db8` (2026-09-04) | 172,784 / 173,216 / 173,628 | 4.67 / 4.69 / 4.68 | 62/62 |
| `8f3c129fa` (2026-09-05) | 126,916 / 173,844 / 126,776 | 8.94 / 8.96 / 8.97 | 114/114 |

Peak RSS is **unchanged** (169.8 MB max against 173.6 MB). The wall doubling is
not a regression: the seed population went 62 → 114 atoms, which is ei-3c
landing. `sep-abduction`, n=4: 123–164 MB, 5.85–5.90 s, 73/73 where the
ledger's old-binary line reads 41/41. Both are already-v2, already-ledgered
atlases and `backfill-ann` has no skip path, so the rewrites are idempotent.

`atlas migrate-all <corpus>`, n=3: **0.08 s**, 26–55 MB peak, stores verified
on disk afterwards. The store half of this job is free.

## Expected price — and the defect that hid half of it

The driver priced the sweep on `_summary.json`'s `tier2_count`. That field
counts **Entities**, which was the whole seed population until ei-3c; since
then the population is whatever the navigation map admits, and the writer's own
marker (`atlas/atoms_ann.population`) reads
`entity,state,claim,configuration,argument,position,summary`. Over all 1,770
`sep-*` atlases:

| denominator | atoms | projected embed time |
|---|---|---|
| `tier2_count` (what the driver said) | 88,801 | 1.8 h |
| seed population (what it embeds) | **190,312** | **~4.0 h** |

2.14×. Verified per-atlas against the two measured runs: preKant
57+17+32+3+6 = 115 against 114 resolved; abduction 33+13+16+3+9 = 74 against
73 — one atom each, the `min_description_chars=10` floor. `position` and
`summary` are declared by the marker but carry no atoms in any SEP atlas yet.

The fix reads the kind list **from the marker the one writer drops**, so the
projection holds no policy of its own to drift from the writer's (ARCH §10.6);
with no marker anywhere it prints `basis: FALLBACK` and names the
underestimate rather than a confident wrong number (ARCH §18.3, §6).

Rate: **13.3 atoms/s**, a two-point fit over two atlases of different size
(73 atoms / 5.87 s, 114 / 8.96 s) = 75.4 ms per atom plus 0.37 s of fixed
per-invocation overhead. ei-3 measured 14.0/s the same way on the pre-ei-3c
population. A third, independent confirmation: a five-corpus smoke on this
binary did 438 atoms in 34 s = **12.9 atoms/s**. So the window is **~4 h of
embed plus ~1.5 min of store builds**, not the order's 2.0–2.5 h, which was
written against the pre-ei-3c population.

## Refusals, before the first batch

Each is an exit **and a `DONE` marker with `state: refused`** — the seat must
be able to tell "refused" from "never started" (ARCH §18.1). All four watched
failing on 2026-09-05:

| Refusal | Trigger |
|---|---|
| stale binary | `$SCLI` or its `-llm` sibling older than the newest Rust/manifest commit |
| missing binary | `$SCLI` or the sibling absent |
| busy box | `pgrep -x cargo/rustc` ≠ 0, **or** MemAvailable < 60 GB. `ALLOW_BUSY_BOX=1` overrides and the override is named in `DONE` |
| no daemon | nothing answering `/v1/models`, or no pid to watch |

`pgrep -x` matches the exact binary name; a phrase match would find this
script's own wrapper shells and refuse against itself.

`claim take daemon:<node>:backfill` is attempted; the claim surface is
banked-broken on this host, so a failure is **named in `DONE` and the run log**
and does not stop the run — the seat's announced window is the real interlock
(ARCH §18.3: the substitution is reported, not silent).

## Stop condition (the order's own)

> Not worth continuing if: the daemon is OOM-killed once during the run — stop,
> cite the journal line, report the ledger position; do not restart and continue
> blind.

The daemon's pid is captured before the first batch and checked after every
batch. If it is gone the driver is stopped there, `DONE` records
`state: oom-stop` with the kernel's own OOM line and the ledger position, and
the run does not continue. A battery that keeps going after its daemon died
produces a plausible, exit-0, wrong ledger. `DONE` carries the
`journalctl -k` OOM grep on **every** path, not only that one, so "the daemon
was not OOM-killed" is evidence rather than an absence of evidence.

## Artifacts

| Path | What |
|---|---|
| `sovereign/bench/sep_atlas/backfill-index.jsonl` | the ledger — **the evidence**, in git, one line per corpus |
| `runs/ei-3b-sep-backfill/DONE` | terminal marker; written by an EXIT trap on every path, refusals included |
| `runs/ei-3b-sep-backfill/markers/leg<N>.rc` | per-leg exit code |
| `runs/ei-3b-sep-backfill/markers/leg<N>-batch-NNN.json` | per-batch marker (leg, batch, progress, free, ledger depth, daemon alive) |
| `runs/ei-3b-sep-backfill/sampler.log` | `free -g` available, one line per batch |
| `runs/ei-3b-sep-backfill/run.log` | full driver stdout/stderr |
| `runs/ei-3b-sep-backfill/measure_rss.sh` | the VmHWM peak-RSS instrument (shape reused from `.wiki-rebuild-scratch/measure.sh`) |

A marker is written only after the batch's per-corpus ledger lines are durable,
so a marker means resume from that point is exact. Markers, `DONE`, `run.log`
and `sampler.log` are cleared at the START of every invocation — before the
refusals — so `DONE` always describes the invocation that wrote it and a
refusal's `DONE` is never read beside a previous run's markers. The ledger is
never touched here; it is the durable record, which is what makes clearing the
rest free.

## The wrapper's own machinery, watched working

The driver has been exercised for real. The wrapper around it had not been, so
it was run against a stub that prints the driver's `progress:` shape and costs
no GPU (`DRIVER=` is overridable for exactly this, and the battery never sets
it). Three findings, all fixed:

| Path | Verdict |
|---|---|
| per-leg batch numbering | **was wrong** — leg 2's `tail -n +1` re-read leg 1's progress lines and reported six batches for three. Each leg now tails from the log's current end. 3 and 3. |
| leg 1 fails → leg 2 must not start | passes — `state: smoke-failed`, `leg1=1`, `leg2=-`, zero leg-2 markers |
| the OOM stop condition | passes — watched firing against a throwaway pid (`WATCH_PID=`, test-only): "DAEMON GONE after leg 1 batch 2", driver killed, `state: oom-stop`, `daemon_alive: false`, and the DO-NOT-RESTART line |

A stop condition nobody has seen fire is not a stop condition (ARCH §18.1).

## Resume

Kill it at any point. The driver reads the ledger and skips any corpus whose
last line is `built`, `had` or `no-seedable`; `failed` is retried. Re-invoking
`run.sh` is the resume.

## Done when

Counts in the ledger — atlases had / built / failed **by name**, for both the v2
store and the ANN table, before and after (24 → N ANN; 692 → N v2); wall time;
rate; model (`Qwen3-Embedding-0.6B-Q8_0`, the daemon's resident 1024-d slot,
which is the space `atlas_navigate_ann` queries in because the loader embeds
through the QUERY-side `EmbedFn`). `ls -d ~/.svrnmesh/indexes/sep-*/atlas/atoms_ann.lance | wc -l`
equals 1,770 minus the failed-by-name list. `atlas_retrieval` walk yield on SEP
recorded twice after (the "before" is walk-dark). SEP retrieval-prod within its
band, two runs (EI2). The daemon was **not** OOM-killed during the run — journal
checked and cited in `DONE`.

## What this script will NOT do

It never starts, stops, restarts or reconfigures the daemon, never swaps or
loads a model, and never edits `~/.svrnmesh/config.toml`. One model resident at
a time is the operator's rule; a busy box or an absent engine is a scheduling
decision reported to the seat.
