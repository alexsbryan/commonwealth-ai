# ei3c-ollama-arm

Order: ei-3c-instruments item 4 (the Ollama arm, never run on this box).
Branch: ei-3c-instruments. Worktree: /home/alexbryan/dev/ei3c-wt.

## Launch

INSIDE the toolbox (llama-server needs Vulkan; ROCm x A3B SEGVs on the host):

    toolbox run -c sovereign-vulkan systemd-run --user --scope -p MemoryMax=14G \
      -- flock /tmp/sovereign-build.lock \
      /home/alexbryan/dev/ei3c-wt/runs/ei3c-ollama-arm/run.sh

NO env is required on the scope. Run 1 (2026-09-07) died in its first second on
`PREFLIGHT: no embed gguf`, because `models/` is gitignored and this is a
worktree: `$REPO/sovereign/models/...` does not exist here and never will.
`corpus-mcp/acceptance.sh` now derives the fallback from the git COMMON dir —
a worktree's is `<main checkout>/.git` — so it resolves
/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf
on its own. An explicit `--setenv=EMBED_GGUF=<path>` still wins if you want a
different model.

The preflight is `acceptance.sh --preflight` (its own dependency list, asked
for by name rather than kept twice here) plus the two things only this unit
needs: an `ollama` on PATH and an installed `sep` index. It refuses with
`PREFLIGHT:` and `preflight rc=1` if it finds itself on the host — all of it
before the first slow leg.

## Forecast

~20 min, ~4 GB peak.
  serve       ~15 s   ollama serve, CPU-only
  pull        ~3 min  qwen3-embedding:0.6b + qwen3:0.6b, ~1.2 GB egress
  acceptance  ~10 min llama-server embed load + the read arm over sep,
                      wessex-hoard, brothers-karamazov-book-1
  named-model ~1 min  the same corpus_list with --embed-model pinned

GPU: none requested. Ollama runs CPU-only (the base Linux tarball ships CUDA
libraries and no ROCm ones, and the daemon owns the GPU). llama-server inside
the toolbox takes Vulkan as it always does — that is the one GPU consumer, and
it is the same one every previous acceptance run used.

## Legs and their markers

Everything lands in `out/`, which is RESET and recreated FIRST — before the
preflight, so a unit that dies in its first second still leaves a readable
record, and so what is in there is always THIS run's. Run 1 left no record at
all; run 2 was reported DONE off a host-side preflight test's leftovers while
the unit was still queued on the build lock. Both are structural now: the reset,
and `--preflight-only`, which writes to a throwaway dir and never touches `out/`.

    run.sh --preflight-only   the dependency table and nothing else, ~1 s

Verified 2026-09-07 under the exact scope form the unit uses
(`toolbox run -c sovereign-vulkan systemd-run --user --scope -p MemoryMax=14G`):
all five checks PASS, exit 0, and `out/` stays absent.

    out/markers.txt   one line per leg, then `DONE rc=<n>`; `signal=TERM` if killed
    out/rc.<leg>      the same rcs one file each
    out/DONE          `DONE rc=<n> <timestamp>`

    preflight  0 = every dependency present         (else exit 2). EVERY check
               writes its verdict to out/preflight.txt, passing ones included —
               run 2's failing check reported only to stderr, which under the
               scope reached neither the journal nor the out dir
    serve      0 = /v1/models answered              (else exit 3)
    pull       0 = both tags pulled                 (else exit 4)
    acceptance the acceptance script's own exit code — 1 FAIL (an assertion it
               lost), 2 REFUSED (a dependency this machine lacks); the unit
               exits with it
    named-model corpus_list with --embed-model pinned

## Artifacts (all under `out/`)

    box-before.txt box-after.txt preflight.txt
    ollama-serve.log ollama-device.txt ollama-pull.log ollama-models.json
    embed-width.txt        the §7 step 6 answer, measured directly
    acceptance.log         the whole run; `acceptance: ollama` lines are the arm
    named-model.jsonl named-model.err

## What the run decides

1. Does the Ollama rung of the discovery ladder resolve live, rather than being
   named by a refusal message on a box that has no Ollama.
2. The embedding width Ollama's `qwen3-embedding:0.6b` returns, against the
   width `sep`'s shipped index was built at — EPISTEMIC_INDEX.md §7 step 6. A
   mismatch degrades that corpus to full-text and must SAY so; either way is a
   measured verdict, not a failure of the host.
3. Whether corpus-mcp's default embedding-model choice (the first id
   `/v1/models` lists, host.rs:364) picks the embedding model when one URL
   serves both chat and embeddings. Leg 4 is the control for that.
