# ei6-acceptance — run manifest

**Order:** `ei-6-distribution` (campaign epistemic-index) · **branch** `ei-6` ·
**worktree** `/home/alexbryan/dev/ei6-wt`

## What it runs

`corpus-mcp/acceptance.sh` twice against a bare `llama-server`:

| Leg | Invocation | Proves | Forecast |
|---|---|---|---|
| 1 | `EMBED_GGUF=… acceptance.sh` | the measured llama-server arm — the three §4 verbs, the discovery ladder, `ask`, the dep-tree closure, and the Ollama arm's verdict *by name* | **5 s measured** (2026-09-05T18:10:58Z) |
| 2 | `ACCEPT_PULL=1 …` | the cold-root pull: `serve --corpus sep` installs from the HF snapshot and serves a cited answer out of it | ~15–25 min, ~875 MB egress |

Leg 1's forecast was **~10 min and the measurement was 5 s** — wrong by two
orders of magnitude, recorded here rather than quietly corrected. The error was
forecasting from ei-5b's ledger, which measures a 20-chapter enrichment against
a live chat model. Leg 1 runs no chat model at all: a 0.6B embedding server
loads in about a second and every corpus it queries is already installed. The
rule the campaign already has — forecast from a ledger of the SAME execution
path — is what I broke; there was no such ledger for this path, and "no ledger"
should have been said rather than a number borrowed from a different one.

Two legs and not one `ACCEPT_PULL=1` invocation: `acceptance.sh` `fail()`s on
the first bad assertion, so a pull that dies on somebody else's uptime would
take the llama-server verdict down with it. Leg 1 is the done-when's measured
arm and must be able to stand alone.

`ACCEPT_INGEST` is deliberately NOT set. ei-5b already measured that path
(5,418 s and 4,796 s on the 35B) and this order changes nothing inside it;
re-running would spend ~90 min re-measuring someone else's result. Accepted by
the seat as a named skip.

## Launch

```sh
EMBED_GGUF=sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf \
  runs/ei6-acceptance/run.sh
```

Runs from the worktree root. Needs the GPU only for the 0.6B embedding model
(one `llama-server`, started and reaped by `acceptance.sh` itself) — no 35B, no
daemon, no chat model.

## Refusals, all before the first long step

| Guard | Floor | Override |
|---|---|---|
| `EMBED_GGUF` set and the file present | — | none (ARCH §18.3) |
| `jq`, `python3`, `curl`, `llama-server` on PATH | — | none |
| `target/debug/corpus-mcp` exists and is **newer than every source under `corpus-mcp/src`** | — | none |
| no `cargo`/`rustc` running | 0 builds | `ALLOW_BUSY_BOX=1` |
| `MemAvailable` | ≥ 20 G | `ALLOW_BUSY_BOX=1` |
| free disk on `/home` | ≥ 120 G | `ALLOW_BUSY_BOX=1` |
| `test-artifacts/ei6-pull-root` does **not** exist | — | `SKIP_PULL=1` |

The stale-binary guard is the one worth naming: measuring a build older than
the diff is this repo's recorded way of validating old code, and `acceptance.sh`'s
own check is only `-x`. The cold-root guard is the other: a warm root would make
leg 2 pass *without pulling*, which is the single thing it exists to prove.

## Output — `runs/ei6-acceptance/<UTC timestamp>/`

- `markers.txt` — per-leg `rc=` lines and a terminal `DONE rc=<worst>`.
  Leg outcomes are read back out of `acceptance.sh`'s own assertion lines
  rather than re-derived, so there is one decider for what each leg said.
- `leg1-llama-server.log`, `leg2-cold-pull.log` — full transcripts.
- `verdicts.txt` — the PASS / FAIL / COULD-NOT-JUDGE / COULD-NOT-RUN / NEVER-RAN lines.
- `box-before.txt`, `box-after.txt`, `walls.txt`.
- `leftover-root.txt` — only if leg 2 died mid-pull; the root is left for
  triage rather than silently deleted.

## Reading the result

`DONE rc=0` is the pass. Three markers are verdicts rather than failures and
must be read, not skimmed:

- `leg1-ollama-arm-COULD-NOT-RUN` — expected on this box (no Ollama installed).
  `leg1-ollama-arm-SILENT rc=1` would be the real failure: the arm printed
  neither verdict.
- `ask(...)` legs report COULD-NOT-JUDGE where the atlas on disk cannot carry
  the bar. That is ei-4's rule and is not this order's to change.
- `leg2-pull-NEVER-RAN` — only when `SKIP_PULL=1` was deliberately set.
