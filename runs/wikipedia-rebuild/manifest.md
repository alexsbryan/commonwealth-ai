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
`AtomId::entity_content_hash`, which keys on `canonical::lookup_key(title)` —
case-folded, punctuation-stripped — and a MediaWiki title IS the identifier.
Fixed in `5219e9971` by minting through a new `AtomId::exact_entity_content_hash`.

### The corpus-wide census — what the guard was standing in front of

The guard aborts on the FIRST collision, so the error names one pair and says
nothing about scale. This is the scale. Method, so it can be re-run: read every
title out of the retiring SQLite (read-only, no build, ~20 s) and group by the
same `lookup_key` the folded constructor used.

```
sqlite: ~/.svrnmesh/indexes/wikipedia/wikipedia_graph.db   (mode=ro)
query:  select title from articles where corpus_id = 'wikipedia'
group:  canonical::lookup_key  — alphanumerics lowercased, every other run
        of characters collapsed to one space, trimmed
```

| | titles | distinct `lookup_key`s | keys holding >1 title | titles merged away |
|---|---|---|---|---|
| whole namespace | 1,562,311 | 1,521,442 | 38,259 | **40,869 (2.62%)** |
| in-scope L5 only | 51,280 | 50,741 | 530 | 539 (1.05%) |

Representative groups, none of them exotic: `{C, C++, C--, &c, °C}`,
`{"+ (album)", "- (album)", "= (album)", Album}`, `{WING, WinG, Wing}`,
`{Polar BEAR, Polar Bear, Polar bear}`, `{TIME (magazine), Time (magazine),
TIME Magazine}`, `{Jigsaw puzzle, Jigsaw Puzzle}` — the pair the build died on.

Two things follow. The collision is in the KEY, before any hashing, so widening
`short_hash` would have moved none of it — the guard's own advice ("widen
wiki_atom_id") was wrong and is corrected in the code. And a build that had
merely been allowed through would have produced a store that looks complete:
1.52M atoms instead of 1.56M, no error, no marker, 40,869 articles quietly
wearing another article's identity and evidence anchor. That is the failure
mode this campaign's guards exist for, and it cost a 72.8 s run to find.

**A caveat this run should carry, not hide.** 26,630 of those 1,562,311 titles
are mojibake in the index itself — UTF-8 decoded as latin-1 at ingest, e.g.
`!XÃ³Ãµ language` for `!Xóõ language` — identically in the SQLite and in
`atoms.json`, so it predates all of this. Their atom ids will be minted from
the corrupted string. Out of this order's scope (ingest, not the store) and
banked as `wikipedia-titles-mojibake-at-ingest`; named here because an exact id
inherits the corruption where the folded one partly absorbed it.

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

**The memory cap is recorded by the run, not by this file.** The launcher sizes
`MemoryMax` from what the box has free at launch, so the manifest cannot know
it — the 40G in the forecast below was an assumption and the real cap may be
lower. Leg 0 writes the effective limit into `$OUT/provenance.txt` by reading
`/sys/fs/cgroup/memory.max` from inside the run's own cgroup, alongside
`MemAvailable` at start. This matters only if the run dies: a kill is
interpretable only against the number that did the killing, and an OOM at 30G
read against a manifest saying 40G yields the wrong finding.

**Safety** No lane, no daemon, no embedding, zero model tokens. Nothing is
written outside `$OUT`. Killing it at any point loses only `$OUT`.
