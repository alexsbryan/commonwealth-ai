# Run manifest: ei-3b-sep-backfill

- **Order:** `.sovereign/features/ei-3b-sep-backfill/order.md`, approved by the
  epistemic-index campaign ladder. Carved from ei-3-index under its park clause
  after the fourth daemon OOM of 2026-09-04. Serves EI4-self-describing-atlas,
  EI2-sep-unchanged.
- **Runs ONLY in a quiet window the seat announces.** A battery: no worker
  builds, no synth lane, no judge calls for its duration.

## What runs

`runs/ei-3b-sep-backfill/run.sh` wraps `sovereign/bench/sep_atlas/backfill_index.sh`.
The driver is the engine and is unchanged in shape — worklist, the two existing
verbs (`atlas migrate-all`, `atlas backfill-ann`), the JSONL ledger and resume
are all its (ARCH §19). The wrapper adds only per-batch markers, a `free -g`
sample per batch, a terminal `DONE`, and the order's OOM stop condition.

Backfills `atoms_ann.lance` across the 1,770 per-article `sep-<slug>` atlases.
Nothing else in a `sep-<slug>` atlas changes. The bare `sep` chunk index is a
control and is not touched, nor are `wessex-hoard` or
`brothers-karamazov-book-1`.

## Precondition: step 0 is landed

**This run must not be started against a binary older than `520aa6db8`.** Step 0
is why the battery is affordable at all. `svrn atlas backfill-ann` used to build
a full `ChatSession`, which loads the wiki graph (51,280 articles, 7.3M edges)
and the meta-atlas (1.57M atoms) into the CLI process beside the resident
daemon; two concurrent invocations were the fifth OOM of 2026-09-04
(`Killed process 4045657 (sovereign-cli-d) ... anon-rss:19842108kB`, 11:52:08).
Measured on one atlas, `/usr/bin/time -v`:

| | peak RSS | wall | result |
|---|---|---|---|
| before (n=1) | 6,911,000 kB | 52.36 s | 62/62 resolved |
| after (n=3) | 172,784 / 173,216 / 173,628 kB | 4.67 / 4.69 / 4.68 s | 62/62 each |

So the CLI's contribution to peak memory is ~0.17 GB, not ~6.9 GB, and
`--batch` stopped being a cost knob (its default came down 100 → 40, for resume
granularity).

## Refusals, before the first batch

Each is an exit, never a warning:

- `< 40 GB` available (`free -g`) → exit 3. Do not lower the floor to fit.
- no daemon answering `/v1/models` → exit 3. Starting one is the seat's call.
- daemon answers but no pid to watch → exit 3.

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
produces a plausible, exit-0, wrong ledger.

## Artifacts

| Path | What |
|---|---|
| `sovereign/bench/sep_atlas/backfill-index.jsonl` | the ledger — **the evidence**, in git, one line per corpus |
| `runs/ei-3b-sep-backfill/DONE` | terminal marker; written by an EXIT trap on every path |
| `runs/ei-3b-sep-backfill/markers/batch-NNN.json` | per-batch exit marker (batch, progress, free, ledger depth, daemon alive) |
| `runs/ei-3b-sep-backfill/sampler.log` | `free -g` available, one line per batch |
| `runs/ei-3b-sep-backfill/run.log` | full driver stdout/stderr |

A marker is written only after the batch's per-corpus ledger lines are durable,
so a marker means resume from that point is exact.

## Resume

Kill it at any point. The driver reads the ledger and skips any corpus whose
last line is `built`, `had` or `no-seedable`; `failed` is retried. Re-invoking
`run.sh` with the same arguments is the resume.

## Expected price

88,801 tier-2 atoms clear the grounding filter; 14.0 atoms/s serial against the
resident 1024-d slot → ~106 min of embedding, plus v2 store builds for the
atlases that lack one. Call it 2.0–2.5 h. The rate is ei-3's measurement, not a
target.

## Done when

Counts in the ledger — atlases had / built / failed **by name**, for both the v2
store and the ANN table, before and after (22 → N ANN; 662 → N v2); wall time;
rate; model. `ls -d ~/.svrnmesh/indexes/sep-*/atlas/atoms_ann.lance | wc -l`
equals 1,770 minus the failed-by-name list. `atlas_retrieval` walk yield on SEP
recorded twice after (the "before" is walk-dark). SEP retrieval-prod within its
band, two runs (EI2). The daemon was **not** OOM-killed during the run — journal
checked and cited in `DONE`.

## What this script will NOT do

It never starts, stops, restarts or reconfigures the daemon, never swaps or
loads a model, and never edits `~/.svrnmesh/config.toml`. One model resident at
a time is the operator's rule; a busy box or an absent engine is a scheduling
decision reported to the seat.
