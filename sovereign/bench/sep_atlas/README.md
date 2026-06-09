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

The script skips any slug whose `~/.sovereign/indexes/sep-<slug>/atlas/atoms.json` already exists. Re-running is safe — only the missing slugs in the bucket get processed.

To force a rebuild of one slug: delete its index dir and rerun.

## Logs

Per-run logs land in `bench/sep_atlas/logs/peer-<i>-<timestamp>.{success,fail,skip,run}.log`. The `.run.log` is the full stdout/stderr of every `sovereign enrich` invocation in that batch — that's where Phase 1 / Phase 8 errors surface.

## Phase gates

Don't fan out to linux-peer until mac-peer's `sep-compatibilism` validation has produced a complete `atlas/` dir (atoms.json, edges.json, plus per-phase artifacts). See `/Users/user/.claude/plans/dapper-imagining-stream.md` for the full plan.

## Fedora (linux-peer) ready-state runbook

Run on fedora before Phase 1 smoke. (Tailscale name: `fedora` at `100.64.0.3`. Sovereign mesh peer name: `linux-peer`.)

```bash
# 1. Verify node_id stable — must match before/after a toolbx restart.
cat ~/.sovereign/node_id | xxd -p
toolbox enter   # or whatever container restart you want to test against
cat ~/.sovereign/node_id | xxd -p   # MUST match the prior value

# 2. Confirm mesh visibility — should show 2 members online.
sovereign-cli mesh status   # or curl localhost:9741/v1/mesh/status

# 3. Cache the SEP parquet locally.
sovereign-cli corpus acquire sep
ls -la ~/.sovereign/indexes/_downloads/sep.parquet

# 4. Confirm philosophy_atlas chat + embed models loaded.
curl -s localhost:9741/v1/models | python3 -m json.tool | grep -E 'Qwopus|Qwen3.5|qwen-embedding'

# 5. End-to-end smoke on a short article.
SHORT_SLUG=$(sovereign-cli enrich sep-ingest --list | sort -k1 -n | head -3 | awk '{print $NF}' | head -1)
sovereign-cli enrich sep-ingest "$SHORT_SLUG"
sovereign-cli enrich build "sep-$SHORT_SLUG"
ls ~/.sovereign/indexes/sep-$SHORT_SLUG/atlas/atoms.json   # exists ⇒ green
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
- `~/.sovereign/indexes/sep-<slug>/atlas/atoms.json` exists
- `sovereign enrich query sep-<slug> "<probe>"` returns results
- `sovereign mesh status` still healthy on both
