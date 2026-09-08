# ei3c-ollama-arm

Order: ei-3c-instruments item 4 (the Ollama arm, never run on this box).
Branch: ei-3c-instruments. Worktree: /home/alexbryan/dev/ei3c-wt.

## Launch

INSIDE the toolbox (llama-server needs Vulkan; ROCm x A3B SEGVs on the host):

    toolbox run -c sovereign-vulkan systemd-run --user --scope -p MemoryMax=14G \
      -- flock /tmp/sovereign-build.lock \
      /home/alexbryan/dev/ei3c-wt/runs/ei3c-ollama-arm/run.sh

The script refuses with `PREFLIGHT:` and rc.preflight=1 if it finds itself on
the host, or if any of ollama / llama-server / jq / python3 / the corpus-mcp
binary / the embed gguf / the `sep` index is missing — all of it checked before
the first slow leg.

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

    rc.preflight  0 = every dependency present     (else exit 2)
    rc.serve      0 = /v1/models answered          (else exit 3)
    rc.pull       0 = both tags pulled             (else exit 4)
    rc.acceptance the acceptance script's own exit code; the unit exits with it
    rc.named-model corpus_list with --embed-model pinned

`DONE` is written on every exit path including SIGTERM (`DONE.reason` says so),
so a killed unit is not mistaken for a hung one.

## Artifacts

    box-before.txt box-after.txt
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
