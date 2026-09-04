# run: wikipedia-rebuild (ei-7c)

**What** Rebuild wikipedia's wiki-class columnar store (`articles.lance` +
`edges.lance`) at full scale with the v2 schema — `atom_id` (content hash) and
`chunk_id` (evidence anchor) — so the store can answer `AtlasProvider`.

**Where** `$OUT`, now defaulting to
`/home/alexbryan/dev/ei7c-wt/runs/wikipedia-rebuild/out` — inside the worker's
own worktree under the gitignored `runs/` tree, not in the seat's checkout.
BESIDE the installed index either way: `~/.svrnmesh/indexes/wikipedia/` is
read-only for this run, and the cutover is a separate seat-approved step that
moves the old atlas aside rather than deleting it.

**Binaries** `$WT/target/debug/sovereign-cli` AND `$WT/target/debug/sovereign-cli-llm`.
Both, because `sovereign-cli` is a dispatcher that `exec`s a sibling and the
`atlas` verb is owned by `sovereign-cli-llm`; rebuilding only the dispatcher is
a silent no-op. The script now REFUSES to start if either is missing, and
writes `$OUT/provenance.txt` (commit, branch, uncommitted-path count, both
binaries' build times) before any leg, so the artifact is attributable to a
commit rather than to a worktree (ARCH §18.5). Built in the worker's own
worktree with its own target dir, so no sibling worker's in-flight changes are
in the measurement.

**Stale markers are cleared at start.** The 2026-09-04 attempt left
`markers/rebuild.rc` = 1 behind. A marker written by a different binary against
different code is not a verdict about this run, and "never ran" has to stay
distinguishable from "failed" (ARCH §18.1). `$M` is removed and recreated
before leg 1.

**Legs + markers** `$OUT/markers/<leg>.rc` per leg, `$OUT/markers/DONE` terminal.
1. `rebuild` — the build, wall + peak RSS to `$OUT/measurements.txt`
2. `sizes` — new vs live on-disk, to `$OUT/sizes.txt`
3. `probe` — neighbors for three real articles, to `$OUT/probe.txt`

**Forecast, from a measured slice.** `wikipedia-fetched`: 1,757 chunks streamed
in 110 ms, built in 806 ms, peak RSS 205 MB, producing 27 in-scope articles /
24,475 edges. Wikipedia is 1,896,488 chunks — 1,079x — so a linear read gives
~2 min streaming and ~15 min building, plus the Lance write of ~1.67M article
rows and ~7.3M edge rows and the `source_title` BTree over them. Call it 20-35
min: it straddles the 25-minute line, which is why this is a RUN-REQUEST rather
than a foreground job.

**Peak RSS is the number I am least sure of and the one that matters.**
`all_chunks_with_raw_metadata()` materialises every chunk record before the
aggregation starts, and the aggregation then holds ~1.67M articles and ~7.3M
edge rows. The slice's 205 MB does not extrapolate cleanly. If it approaches the
box's headroom the run should be stopped and the build made streaming — that is
the finding, not a failure.

**Not measured by GNU time.** `/usr/bin/time` is not installed in the
`sovereign-vulkan` toolbox; the script reads `/proc/<pid>/status` `VmHWM`, the
same kernel high-water mark `time -v` reports as "Maximum resident set size".

## What changed since the failed attempt (2026-09-04 14:54, rc=1)

The first attempt reached leg 1, streamed all 1,896,488 chunk records in
1,862 ms, and was REFUSED by the build's own collision guard 72.8 s in at
peak RSS 7.2 GB against a 40 GB cap:

```
error: build: wiki atom id collision: entity-6b8aef7e6205164a is both
"Jigsaw puzzle" and "Jigsaw Puzzle" in corpus wikipedia
```

That is not a memory failure and not a scale failure — it is the guard doing
its job on a real defect. `wiki_atom_id` minted through
`AtomId::entity_content_hash`, which case-folds the name, and Wikipedia titles
are case-distinct identifiers. Measured over the whole live title namespace
(1,562,311 titles): 38,259 folded keys carry more than one title and 40,869
titles — 2.62% — would have been silently merged into another article's atom.
Fixed in `5219e9971` by minting through a new `AtomId::exact_entity_content_hash`.

**Two numbers from that attempt are real and carry forward as measurements,
not forecasts.** Streaming the full corpus costs 1.9 s, not the ~2 min the
forecast below guessed. Peak RSS reached 7.2 GB by the time the aggregation
finished and the columnar build began — comfortably inside the 40 GB cap, which
retires the "peak RSS is the number I am least sure of" worry for the streaming
and aggregation halves. It says nothing yet about the Lance write.

**The binary-is-stale worry is answered by the guard itself, not by an mtime
check.** If the binaries handed to this run predate `5219e9971`, leg 1 fails
again with that same collision error — loudly, in seconds, before writing
anything. There is no path where a stale binary silently mints 1.67M ids under
the old scheme.

**Safety** No lane, no daemon, no embedding, zero model tokens. Nothing is
written outside `$OUT`. Killing it at any point loses only `$OUT`.
