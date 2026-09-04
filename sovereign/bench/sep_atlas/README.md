# SEP atlas — two-peer parallel campaign

Driver + logs for running `philosophy_atlas` enrichment over the Stanford Encyclopedia of Philosophy in parallel across two mesh peers (mac-peer + linux-peer). Per-article granularity — each `sep-<slug>` is a self-contained sub-corpus, no atom-ID coordination across peers.

## Why this layout

- `mesh_sharing=false` in the SEP recipe (Stanford license) ⇒ no distributed-shard ingest of the base index.
- Atlas pipeline is per-corpus today ⇒ running it on the same corpus from two nodes would collide on atom IDs.
- Per-article enrichment naturally parallelizes because each `sep-<slug>` is a separate corpus.

## Usage

Both peers run the same command with their own `--peer-index`. The hash mod 2 of each slug determines which peer processes it; the cover is disjoint.

```bash
# On mac-peer
sovereign enrich sep-ingest --list \
  | bench/sep_atlas/run_batch.sh --peer-index 0 --limit 5

# On linux-peer (over ssh / tailscale)
sovereign enrich sep-ingest --list \
  | bench/sep_atlas/run_batch.sh --peer-index 1 --limit 5
```

Both peers must have the SEP parquet cached locally (`sovereign corpus acquire sep`) and the `philosophy_atlas` chat + embed models loaded.

### Flags

| flag | purpose |
|---|---|
| `--peer-index 0\|1` | required. Determines which hash-bucket this peer owns. |
| `--slugs FILE` | read slug list from FILE instead of stdin. |
| `--limit N` | stop after N matched-bucket slugs. Use during smoke runs. |
| `--dry-run` | print what would be run, don't actually invoke `sovereign enrich`. |

### Idempotence

The script skips any slug whose `~/.svrnmesh/indexes/sep-<slug>/atlas/atoms.json` already exists. Re-running is safe — only the missing slugs in the bucket get processed.

To force a rebuild of one slug: delete its index dir and rerun.

## Logs

Per-run logs land in `bench/sep_atlas/logs/peer-<i>-<timestamp>.{success,fail,skip,run}.log`. The `.run.log` is the full stdout/stderr of every `sovereign enrich` invocation in that batch — that's where Phase 1 / Phase 8 errors surface.

## Phase gates

Don't fan out to linux-peer until mac-peer's `sep-compatibilism` validation has produced a complete `atlas/` dir (atoms.json, edges.json, plus per-phase artifacts). See `/Users/user/.claude/plans/dapper-imagining-stream.md` for the full plan.

## Fedora (linux-peer) ready-state runbook

Run on fedora before Phase 1 smoke. (Tailscale name: `fedora` at `100.64.0.3`. Sovereign mesh peer name: `linux-peer`.)

```bash
# 1. Verify node_id stable — must match before/after a toolbx restart.
cat ~/.svrnmesh/node_id | xxd -p
toolbox enter   # or whatever container restart you want to test against
cat ~/.svrnmesh/node_id | xxd -p   # MUST match the prior value

# 2. Confirm mesh visibility — should show 2 members online.
sovereign-cli mesh status   # or curl localhost:9741/v1/mesh/status

# 3. Cache the SEP parquet locally.
sovereign-cli corpus acquire sep
ls -la ~/.svrnmesh/indexes/_downloads/sep.parquet

# 4. Confirm philosophy_atlas chat + embed models loaded.
curl -s localhost:9741/v1/models | python3 -m json.tool | grep -E 'Qwopus|Qwen3.5|qwen-embedding'

# 5. End-to-end smoke on a short article.
SHORT_SLUG=$(sovereign-cli enrich sep-ingest --list | sort -k1 -n | head -3 | awk '{print $NF}' | head -1)
sovereign-cli enrich sep-ingest "$SHORT_SLUG"
sovereign-cli enrich build "sep-$SHORT_SLUG"
ls ~/.svrnmesh/indexes/sep-$SHORT_SLUG/atlas/atoms.json   # exists ⇒ green
```

## Phase 2 smoke (one article each, in parallel)

When mac-peer and linux-peer are both green on a single article, run two distinct slugs concurrently:

```bash
# mac-peer
sovereign enrich sep-ingest --list \
  | bench/sep_atlas/run_batch.sh --peer-index 0 --limit 1

# linux-peer (separately, same time)
sovereign enrich sep-ingest --list \
  | bench/sep_atlas/run_batch.sh --peer-index 1 --limit 1
```

Verify on each peer:
- `~/.svrnmesh/indexes/sep-<slug>/atlas/atoms.json` exists
- `sovereign enrich query sep-<slug> "<probe>"` returns results
- `sovereign mesh status` still healthy on both

## The INDEX backfill (`backfill_index.sh`) — a different job from the one above

`run_batch.sh` BUILDS atlases (`enrich sep-ingest` + `enrich build`, LLM work,
hours per peer). `backfill_index.sh` completes atlases that already exist: the
v2 store (`atoms.lance` + `edges.csr`) and the ANN seed table
(`atoms_ann.lance`). No extraction, no chat model — one embed call per atom.

Since ei-3-index (2026-09-04) `atoms_ann.lance` is a mandatory artifact of the
atlas WRITE, so nothing built from that commit forward needs this. It exists
for the ~1,770 SEP per-article atlases built before it, of which **22 had a
seed table and 662 had a v2 store**. An atlas without the table loads,
enumerates, reports nothing wrong and cannot ground — every answer over it
falls back to cosine over chunks.

```bash
./backfill_index.sh --dry-run              # worklist + the projected price
./backfill_index.sh                        # every sep-* atlas on this host
./backfill_index.sh --limit 20             # smoke run
./backfill_index.sh brothers-karamazov-book-1
```

It is a DRIVER over two existing verbs — `svrn atlas migrate-all <id>` for the
store, `svrn atlas backfill-ann <ids...>` for the table. No filter, threshold
or table is re-derived here; the one writer stays
`corpus_engine::enrichment::atlas::context_loader::backfill_ann`.

**Measured price on the Halo (2026-09-04, before any run):** 88,801 atoms clear
the production grounding filter across the 1,770 `sep-*` atlases (the sum of
`tier2_count` over their summaries, which is the same population the filter
admits); embed throughput **14.0/s** serial keep-alive against the resident
1024-d slot (40 calls, 71.2 ms each); 1,108 atlases need their store built
first. **~106 min of embedding**, plus store builds and ~40 s of session
bootstrap per CLI invocation (`--batch`, default 100, amortises that).

**It does not self-throttle, because there is nothing to throttle.**
`load_atlas_context` awaits one embed call at a time — the job is strictly
serial by construction. That matters on this host: a parallel embed fan
OOM-killed the daemon on 2026-05-31, and the daemon was OOM-killed four more
times on 2026-09-04. The only load knob is "stop", and the ledger makes
stopping free.

**Ledger + resume.** `backfill-index.jsonl` (beside this README, in git — the
`logs/` dir is gitignored) gets one line per corpus the moment its batch
reports: corpus, state (`built` / `no-seedable` / `failed`), whether the v2
store was `had` or `built`, rows/of, seconds. A re-run skips anything already
recorded done and retries `failed`. A corpus with no line in its batch's output
is `failed`, never counted as done. The closing table is had / built / failed
**by name**.

Status: the SEP sweep is parked as its own order (ei-3b, seat decision
2026-09-04 — 106 min of sustained embed on a box that killed the daemon four
times that day). `brothers-karamazov-book-1` is in the ledger: 16/16 atoms
embedded, v2 store already present, 49 s wall of which ~1 s was embedding.
