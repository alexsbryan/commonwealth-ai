# run: ei5b-stage2 — `corpus ingest` on the 35B against two bare llama-server endpoints

Order ei-5b-build-verb; EPISTEMIC_INDEX.md §4 and §7 step 5. Staged for the
run channel: a 20-chapter enrichment against a llama-server exceeds the
25-minute in-session ceiling, and this runs TWICE.

## Invocation (the seat launches this)

From `/home/alexbryan/dev/ei5b-wt`, INSIDE the `sovereign-vulkan` toolbox.
Inside is not a preference: `llama-server --list-devices` resolves ROCm on the
host and Vulkan in the toolbox, and the recorded hazard on this box is a SEGV
on ROCm x A3B. The stage-2 chat model is an A3B.

    toolbox run -c sovereign-vulkan systemd-run --user --scope -p MemoryMax=40G \
      --setenv=CHAT_GGUF=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3.6-35B-A3B-MTP-UD-Q6_K.gguf \
      --setenv=EMBED_GGUF=/home/alexbryan/dev/commonwealth-ai/sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf \
      -- ./runs/ei5b-stage2/run.sh

## Forecast, from measured volumes rather than estimate

The daemon-built control is the same corpus, same 20 chapters, same phases.
Its ledger (`~/.svrnmesh/enrichment/wessex-hoard-ei{3,4}/_tokens.json`), two
independent builds on the 35B:

| build | calls | prompt | completion | wall |
|---|---|---|---|---|
| ei-3 | 20 | 53,670 | 17,940 | 333 s |
| ei-4 | 20 | 53,670 | 17,532 | 360 s |

The daemon serves those concurrently; a bare llama-server does not. At the
35B-A3B's recorded single-stream rate on this box (~14.8 tok/s), 17.9k
completion tokens is ~20 min of generation; 53.7k of prefill adds 1.5-3 min;
the 30 GB model loads in ~3 min; the acceptance's other legs (corpus_search,
atoms_lookup, `ask` on wessex-hoard and brothers-karamazov-book-1, two scorer
passes, the dep-tree assertion) added ~5 min on the 4B probe. So one run is
**30-45 min**, and this manifest runs two: **FORECAST 60-95 min**, servers
reloaded between them.

MEMORY: ~31 GB of 35B weights + ~1 GB embed + corpus-mcp, under a 40G cap.
This needs the daemon's 35B NOT resident — it is lazy-loaded and idle-unloads
at 30 min, so an unloaded primary beside this is the same memory shape as a
config swap. Peak box use ~37 GB.

DISK: each run writes a new corpus under `~/.svrnmesh/indexes/`; the 4B probes
were tens of MB each. Negligible against the 100 GB campaign stop.

## Legs and their markers

`markers.txt` gets one `<name> rc=<n> <utc>` line per leg and a terminal
`DONE rc=<n>`. **No DONE line means the run died, and the last marker names
where.** `walls.txt` gets one wall per run; `verdicts.txt` collects the recall
rows and PASS/FAIL lines; `box-before.txt` / `box-after.txt` record memory,
disk and build count so a wall explains itself.

| marker | what it proves |
|---|---|
| `preflight-env` | both GGUFs were supplied AND exist — not defaulted |
| `control-guard` | no run id is `wessex-hoard`; the control is never written |
| `acceptance-N(id)` | that run's own exit code — the verdict |
| `runN-leg-embed-server` / `-chat-server` | both llama-servers became healthy |
| `runN-leg-ingest` | acquire → extract → chunk → embed → index → 8 enrichment steps |
| `runN-leg-degradation-named` | the run PRINTED its degradations rather than hiding them |
| `runN-leg-recall` | the truth.json recall table was produced, or COULD-NOT-JUDGE named |
| `runN-leg-ask` | `ask` ran on the new corpus and on the installed fixtures |
| `runN-leg-dep-tree` | `cargo tree -p corpus-mcp` free of llama.cpp / ort / iroh |

## The bar

`truth.json` recall against the daemon-built `wessex-hoard`, re-measured at
this branch's tip on 2026-09-04, rc 0, 159 atoms, fingerprint
`sha256:22c90ef2a1aa332e459d711c5e3cf692a5be1602d7e519788b21e3d1c0c0b247`:

    catalogue_ref 7/7 · coin family 19/7 · mint 3/3 · ruler 4/4
    attribution 49/7 · grade 3 of 4 declared (die-link never extracted)

Control and run share an embedding model, so the comparison is not confounded
by embedding space: the control's `_corpus_meta.json` reads
`Qwen3-Embedding-0.6B-Q8_0` / 1024 dimensions, and that GGUF's own metadata
reads `qwen3.embedding_length = 1024`.

The scorer is `scripts/setup-numismatics-corpus.sh --atlas <id>
--recipe-unchanged`, called by acceptance.sh once per atlas. `--atlas` implies
assert-only, so the scorer needs no built binary; `--recipe-unchanged` is
required because a git checkout resets mtimes and the staleness guard would
otherwise report COULD-NOT-JUDGE on a file nobody edited. It PRINTS that
assumption, and the structural type-name comparison still runs.

## What the log will say, and why it is not a defect

The bare-endpoint path has no GLiNER — the entity pass is the chat model's —
and uses `response_format: json_schema` rather than an OICP structured-output
mode. Both are printed by the run and asserted on. On a thinking model, phase
1 returns an empty `content` whenever the chain of thought does not close
inside the budget: llama-server puts the reasoning in `reasoning_content`, so
an exhausted budget yields an empty string rather than an error. The auto-retry
recovers it terse at double the budget. Measured on the 4B (2026-09-04): 2 of
3 chapters failed first pass, which is why stage 1 was stopped and the 35B is
the engine here — it is also the engine the control was built on.
