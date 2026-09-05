# ei6-restore-probe — run manifest

**Order:** `ei-6-distribution` · **branch** `ei-6` · **worktree**
`/home/alexbryan/dev/ei6-wt` · staged per the seat's directive of 2026-09-05
("the cheap experiment: yes … one leg, cold root, pull to the restore decision
with stderr surviving, and let the NEW refusal be what stops it").

## The question

Does `corpus serve --corpus sep` on a cold root **restore** the prebuilt
snapshot, or **discard** it and rebuild the corpus from source?

Run `20260905T181423Z` did the latter for 93 minutes and was OOM-killed at a
14 GB cap (`journalctl --user -u ei6-acceptance`: unit exits 143 at 12:47:52,
and the in-toolbox scope `run-p870359-i17690911.scope` reports
`Failed with result 'oom-kill'` one second later — 14 GB peak, 603 MB swap,
42 min CPU over 1 h 33 m wall). The cap was sized for a pull-and-extract; what
ran was a full embed of ~182k paragraphs. The mechanism had to be
reconstructed from file mtimes, because the serve's stderr went to a
trap-deleted mktemp.

## Launch

```sh
cd /home/alexbryan/dev/ei6-wt && \
EMBED_GGUF=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf \
  runs/ei6-restore-probe/run.sh
```

Forecast **~8–14 min**, mem ~2 GB. One `llama-server` on the 0.6B embedding
model, started and reaped by `acceptance.sh`; **no daemon, no 35B** — it does
not touch the judge window. Egress ~875 MB, authorized in the order under
`ACCEPT_PULL`.

## What stops it — and why that is the experiment

`PULL_DEADLINE_MINS=12` makes **corpus-mcp's own refusal** the thing that ends
a fall-through, not the unit's cap. A restore measured ~6 min, so 12 allows a
restore and refuses a rebuild.

**If the run ends by OOM or `RuntimeMaxSec` instead, the fix did not hold, and
that is the verdict.** Do not read a kill as a pass. A `DONE rc=143
KILLED-BY=SIGTERM` line now exists precisely so a killed run cannot be mistaken
for one still going.

## The four outcomes, each its own marker

| Marker | Means |
|---|---|
| `VERDICT-RESTORED-AND-SERVED rc=0` | the snapshot restored and served a cited answer — pull-if-absent **met** |
| `VERDICT-REFUSED-BY-DEADLINE rc=0` | it fell through to a rebuild and **the new bound stopped it**. The defect is confirmed AND contained; pull-if-absent is not met, but no user waits hours |
| `VERDICT-FELL-THROUGH-CAUGHT-BY-ASSERTION rc=0` | the log named the discard and `acceptance.sh` failed the leg on it — same finding, caught one layer earlier |
| `VERDICT-UNCLASSIFIED rc=1` | none of the above matched; read `probe.log` and the captured `.pull.err` before concluding anything |

`indexes-sep-present` distinguishes a restored index from one that was
extracted and then deleted by the probe-failure path — the signature that
identified the original failure.

## Output — `runs/ei6-restore-probe/<UTC>/`

`markers.txt` (terminal `DONE`, on kill too), `probe.log`,
`ei6-probe-root.pull.err` (**the serve's own stderr — the artifact this run
exists to capture**), `walls.txt`, `box-before/after.txt`, `root-size.txt`,
`root-indexes-head.txt`.

The cold root `test-artifacts/ei6-probe-root` is left in place for triage. The
previous run's root is preserved beside it as
`ei6-pull-root.discarded-20260905T181423Z` (1.8 GB, 1,770 orphaned
`sep-<slug>` atlas dirs from the discarded restore — banked as
`prebuilt-discard-leaves-orphan-sibling-dirs`).
