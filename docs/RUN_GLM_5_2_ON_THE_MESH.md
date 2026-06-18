# Run GLM-5.2 on the mesh

GLM-5.2 is a 744-billion-parameter open-weight model. No single machine most of
us own can hold it — the Q4 quant is **436 GB** on disk. But only about **40
billion** of those parameters do any work on a given token; the other ~700B sit
resident, waiting their turn. That gap is the whole opportunity. You need enough
*memory* across a group of machines to hold all 436 GB at once, but the *work*
per token stays at roughly the speed of a 40B model. Pool four ordinary 128 GB
machines and you can run, between you, a frontier model none of you could run
alone.

This is the same distributed-inference path described in
[Run a model bigger than your machine](./RUN_A_BIGGER_MODEL.md): one host, a few
workers, the model's layers split across them, and you talk to the host as if it
were running locally. This page is the concrete version for GLM-5.2 — what to
bring, what to type, and what's honestly true about it today, so you can hand it
to the people you want to pool machines with.

## Two things that are true today

Worth knowing before you recruit anyone, because they shape what you're
promising.

**It loads on what we ship — but it runs dense.** GLM-5.2's architecture
(`glm_moe_dsa`) has been in mainline llama.cpp since
[PR #19460](https://github.com/ggml-org/llama.cpp/pull/19460) (merged February
2026), and Sovereign's pinned build already includes it — so the model loads
today, no llama.cpp bump required to start. What's *not* wired up yet is the
sparse-attention "indexer," the part that makes GLM-5.2's 1M-token window cheap by
attending to only part of the history. Until that lands upstream (no release has
it as of mid-2026), llama.cpp runs attention the ordinary dense way: correct, but
a long context costs what it would on any dense model. Size the KV cache for the
context you actually use and **start modest — 32–64k, not the headline million.**
Still worth doing before you recruit: confirm one host can `sovereign chat`
against the model before you ask four people to clear 110 GB of disk.

**For long context, build against current llama.cpp.** Running GLM-5 dense
surfaced an overflow: past roughly 200k tokens the dense attention mask exceeds a
2 GiB limit and you get a crash or gibberish
([#23574](https://github.com/ggml-org/llama.cpp/issues/23574), fixed late May
2026). The fix rides in `llama-cpp-4` 0.3.1; older builds — ours included at time
of writing — can still trip it on very long prefills. At the modest context above
you won't reach it, but if you mean to push the window, make sure the cluster is
built against 0.3.1 or newer.

## What the group needs to bring

The model's 436 GB of weights have to be resident in memory across the machines,
all at once, plus room on top for the KV cache and compute buffers — and, on the
host, for the small fast/embed models it keeps running locally (those don't
distribute). Budget around **480–500 GB of pooled memory**. In practice:

- **Four 128 GB machines** (512 GB) is the clean target — Strix Halo boxes,
  128 GB Macs, or a mix. It clears the weights with ~70 GB to spread across the
  group for KV and buffers. If your host is busy or you want bigger contexts, a
  **fifth** node buys breathing room.
- **Five ~96–100 GB machines** gets you to the same place with smaller boxes.
- A **512 GB Mac Studio** can nearly host alone; pair it with one more node and
  you get the fewest network hops, which is the fastest this gets.

Memory is the pool; disk is per-machine. The **host** holds the whole GGUF —
**436 GB of free disk**, all ten shards co-located. The **workers don't**: with
byte-range fetch (below) each worker pulls only its own slice, about
**`436 / N` GB** — roughly 110 GB each across four nodes.

And a network path between the machines. On a LAN you have it. Across locations,
[Tailscale](https://tailscale.com) gives them a private address space and the
mesh rides on top; a direct LAN or wired link is meaningfully faster for the
per-token traffic than Tailscale-over-Wi-Fi.

## Set it up

**1. Get the model onto the host.** All ten shards, co-located:

```bash
huggingface-cli download unsloth/GLM-5.2-GGUF \
  --include "UD-Q4_K_S/*" \
  --local-dir ~/.sovereign/models/GLM-5.2
```

**2. Point the host's primary slot at the first shard.** llama.cpp follows the
rest of the split on its own, which is why they must sit in one directory. In
`~/.sovereign/config.toml`:

```toml
[models]
primary = "/home/you/.sovereign/models/GLM-5.2/UD-Q4_K_S/GLM-5.2-UD-Q4_K_S-00001-of-00010.gguf"
```

**3. Put the machines on one mesh.** On the host:

```bash
sovereign mesh create        # prints a key like cwth-a1b2-c3d4-e5f6
```

On each other machine:

```bash
sovereign mesh join cwth-a1b2-c3d4-e5f6
```

`sovereign mesh status` should list them all.

**4. Start each lending machine as a worker.** It doesn't need the model on
disk; it'll fetch its slice.

```bash
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 sovereign daemon run
```

**5. Start the host with discovery and byte-range fetch on.**

```bash
SOVEREIGN_RPC_DISCOVER=1 \
SOVEREIGN_RPC_SHARD_FETCH=ranges \
  sovereign daemon run
```

`SOVEREIGN_RPC_SHARD_FETCH=ranges` is the one that matters at this size: without
it, any worker that doesn't already hold the model would try to fetch the entire
436 GB GGUF. With it, each worker materializes only its own ~110 GB slice. The
host finds the workers, computes one split weighted by each machine's memory,
seeds every worker the shard it's responsible for, and loads. When it's ready,
you query the host exactly as you always do.

**Peers on a metered connection?** Hand them the GGUF on a drive and have them
warm their cache offline, so nothing crosses their ISP link at load time:

```bash
sovereign mesh warm-cache <model.gguf>     # builds the cache locally, no network
```

[RPC_DISTRIBUTED_INFERENCE.md](./RPC_DISTRIBUTED_INFERENCE.md) covers warming and
the tuning knobs in full.

## Optional: make it a first-class named model

Setting `[models] primary` to the path is enough to run it. If you'd rather it
carry proper metadata — capabilities, a display name, the right family — add a
cluster profile to `sovereign/models.toml`, mirroring the existing distributed
entries. There's no GLM family in Sovereign yet, so it loads like MiniMax does:
`Unknown` family, with the chat template read from the GGUF and generic sampling.
(A dedicated `Glm` family for tuned sampling defaults is a clean follow-up.)

```toml
# Distributed-only — too big for one node, never auto-selected by hardware
# detection. Runs only across the mesh via RPC layer-distribution.
[profiles.cluster_glm52.thoughtful]
repo      = "unsloth/GLM-5.2-GGUF"
file      = "UD-Q4_K_S/GLM-5.2-UD-Q4_K_S-00001-of-00010.gguf"
family    = "Unknown"          # no Glm family yet; GGUF carries the chat template
quant     = "UD-Q4_K_S"
size_gb   = 436.0
thinking  = true
hf_url    = "https://huggingface.co/unsloth/GLM-5.2-GGUF"
base_name = "GLM-5.2"
[profiles.cluster_glm52.thoughtful.capabilities]
general     = 4
analysis    = 4
code        = 4
instruction = 4
math        = 4
creative    = 4
```

## What to expect

**The first load is the expensive part, once.** Each worker pulls its ~110 GB
slice over the network the first time (or reads it from a pre-warmed cache, with
no transfer at all). After that the slice stays resident; per answer, only a few
kilobytes of intermediate state cross the wire between layers. The cost is paid
up front and amortizes — which is exactly why this earns its keep on a model that
can't fit one machine.

**It runs at a measured pace.** Layers are split across the machines in a
pipeline, so a token walks from node to node and crosses the network at each
boundary — that, not raw compute, is the ceiling. The 40B-active design keeps the
arithmetic light; the network and the hop count set the speed. For a reference
point from the same path: MiniMax-M2.7 (140 GB, ~10B active) runs around 17 tok/s
on a two-node Strix cluster. GLM-5.2 is larger, spread over more nodes with more
hops, and — until the sparse-attention path lands — pays full dense attention, so
expect **single digits on a good local network, slower across the internet**. We
haven't measured it on a real cluster yet; that's the first thing to do once the
machines are pooled. A direct wired link or 10GbE between co-located boxes is the
biggest lever you have on it.

**To see the split is real,** watch a worker during a query — its memory holds
roughly its share of the model for the length of the answer, and its daemon logs
an accepted connection held for the duration. tok/s alone won't tell you;
resident memory will.

## If a machine drops

Workers come and go and the mesh expects it. If one leaves — a closed laptop, a
dropped link — the host notices, sets it aside, and reloads on the machines still
present; you get the model running on what's left rather than a hung process or a
wrong answer, and the worker is folded back in once it's been steadily reachable
again. One that keeps flapping is benched for a cooldown rather than trusted
straight back in.

One sharp edge worth saying plainly to the people you recruit: a worker that
crashes *mid-answer* can take the host process down with it — an upstream
llama.cpp limitation, not something we paper over. The host restarts in seconds
and the cluster re-forms on its own, but only if it's running under a supervisor.
So run it as a service:

```bash
sovereign install-service
```

That's the intended way to run a cluster unattended, and it's the difference
between "a node rebooted" being a non-event and being a phone call.

---

Four machines, one mesh, and a model the size of a small data center's worth of
weights answers from your own hardware.

*The mechanism, the warming workflow, and every tuning knob live in
[RPC_DISTRIBUTED_INFERENCE.md](./RPC_DISTRIBUTED_INFERENCE.md). The plain,
model-agnostic version of this story is
[Run a model bigger than your machine](./RUN_A_BIGGER_MODEL.md).*

### References

- Model card and GGUF quants — [unsloth/GLM-5.2-GGUF](https://huggingface.co/unsloth/GLM-5.2-GGUF)
- Architecture (744B / ~40B active, 256+1 experts, 78 layers, DSA, 1M ctx) — [vLLM recipe, zai-org/GLM-5.2](https://recipes.vllm.ai/zai-org/GLM-5.2)
- llama.cpp `glm_moe_dsa` support (indexer not yet wired) — [ggml-org/llama.cpp #19460](https://github.com/ggml-org/llama.cpp/pull/19460)
- GLM-5 long-context dense-mask overflow + fix — [#23574](https://github.com/ggml-org/llama.cpp/issues/23574) / [#23610](https://github.com/ggml-org/llama.cpp/pull/23610) (in `llama-cpp-4` 0.3.1)
