# Archived finding: process replicas vs in-process batching (2026-07-20)

**Conclusion: for a model that fits one box, spawning N child-process replicas
is the WRONG way to get throughput — in-process multi-sequence batching wins.**
This directory is the archived evidence for that conclusion. The `svrn bench
replicas` command and the `ReplicaPoolProvider` it exercised were **removed**
afterward (the compute-child boundary was pared down to the single-child path it
actually needs — see `DISTRIBUTED_PILOT_READINESS.md` P1). The JSON receipts are
kept as raw data; they cannot be regenerated (the tool is gone).

## What was measured

A K-concurrency sweep against the daemon's `/v1/embeddings` (Qwen3-Embedding-0.6B,
Strix Halo), batch=1, comparing the in-process embed slot to a CPU embed pool at
N=1 and N=4 replicas.

| Config | K=1 | K=8 | speedup | latency shape |
|---|---|---|---|---|
| E0 in-process slot (GPU) | 12 texts/s | 12 texts/s | flat | linear 665→5274 ms |
| E1 pool N=1 (CPU) | 13 texts/s | 15 texts/s | flat | linear 75→536 ms |
| E3 pool N=4 (CPU) | 13 texts/s | 26 texts/s | **1.97×** | flat to K=4 |

Receipts: `results/e0-live.json`, `results/e1-n1-cpu.json`, `results/e3-n4-cpu.json`.

## Why replicas lose

The N=4 pool scaled to only ~1.97×, not 4×: the four children each grabbed all 16
CPU cores and thrashed (oversubscription). And even a clean 4× would still be
*worse* than a single in-process multi-sequence decode, which:

- issues one batched kernel instead of N processes fighting one device;
- pays no process-spawn / localhost-HTTP hop;
- holds one copy of the weights, not N;
- has no thread oversubscription.

`FastShortCoalescer` already does this for short completions, and
`EmbeddedLlamaCpp::embed_batch` does it for embeddings — so the E0 baseline's
flatness was the `/v1/embeddings` handler calling `embed` per input, not a
fundamental limit. (That handler now issues one `embed_batch`.) FastShort's real
gaps — no streaming, 6000-char cap, lockstep head-of-line — are closed by
extending in-process batching to the primary + streaming, not by spawning
processes.

## What the process boundary is still for

Crash isolation (a ggml `SIGABRT` kills only the child) and the
can't-fit-one-box distributed case. Neither is throughput parallelism.
