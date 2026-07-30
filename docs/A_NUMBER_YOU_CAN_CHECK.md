# A number you can check

One model, two mismatched machines, one measured number — and the exact way to
get a different number on your own hardware. Nothing on this page is estimated,
extrapolated, or averaged into existence: every figure below came out of
`svrn mesh bench`, and the same command produces yours.

## The claim

**Qwen3.5-122B-A10B** (Q5_K_XL quant, ~86 GB of weights, file
`Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003.gguf`), split across an AMD
mini-PC and a Mac on one flat home LAN — no special interconnect:

| | measured |
|---|---|
| Decode | **8.4 tok/s** — median of 4 runs, observed 7.8–11.1 |
| Time to first token | 2.5 s |
| Inter-token latency (p50) | 93 ms |
| Context length | 32,768 |
| Split | 36 layers + output head on the host · 12 layers on the worker |

What that feels like: tokens arrive a little faster than most people read.

The headline is the **median run**, and every companion figure comes from that
same run — we don't stitch a best-of composite. One of the four runs came in
30% above the band, coherently faster on every metric at once; we couldn't
attribute it, so it widens the observed range and does not set the headline.
Your number should land near the median.

## The hardware

| Node | GPU / backend | Memory |
|---|---|---|
| Host — AMD Strix Halo | Radeon 8060S, Vulkan (RADV) | 128 GB unified |
| Worker — Apple Silicon Mac | Metal | 64 GB unified |

Different vendors, different GPU backends, one model. The worker cannot run
this model alone — 86 GB of weights against 64 GB of memory — and never holds
the file on disk: at load it is handed its 12-layer slice over the mesh, keeps
it resident, and after that only a few kilobytes of state cross the wire per
answer.

## Reproduce it

The long-form walkthrough is [Run a model bigger than your
machine](./RUN_A_BIGGER_MODEL.md); condensed:

```sh
# on each machine
curl -fsSL https://svrnme.sh/install.sh | sh

# host
svrn mesh create                                  # prints a join key

# worker
svrn mesh join <key>
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 svrn daemon run # lends its GPU to the mesh
```

Before you download anything or commit to anything, ask what your mesh can do:

```sh
svrn mesh plan the-model.gguf --from-mesh
```

That reads the GGUF header — no model load, no GPU, instant even on a 400 GB
file — and reports per-machine fit, the block split, and the network hops the
split would cost. If one machine's share won't fit, it says which machine and
what would fix it. Then point the host's `[models] primary` at the file, start
it with `SOVEREIGN_RPC_DISCOVER=1`, and once it's serving:

```sh
svrn mesh bench
```

Three timed trials against the configuration you are actually running, filed
with the conditions the run met. Match `svrn` versions across the mesh, and
give the bench a quiet box — a machine busy compiling will fail the
steady-state gate, and the run will be refused rather than filed.

## Getting a different number is the point

The claim is not that your hardware will match ours. It's that the same
command tells the truth about *yours*. On your mesh, `svrn mesh plan` quotes a
speed only when that exact configuration — same machines, same split, same
link class, same context — has been measured, and names where the number came
from. Anything else and it says **not measured**, tells you what nearby
configuration *was* measured and how yours differs, and shows the command that
takes the measurement. A peer's measurement is always attributed
("Measured by …"), never presented as your machine's own.

Some things we deliberately refuse to do, because a wrong number here costs
you a hardware decision:

- **No extrapolation.** A 122B figure is never scaled from a smaller model's
  benchmark. We tried a size-law rate card; it was wrong on five of six
  simulated fleets, so it's not in the product.
- **No silent averaging of bad runs.** A run that wasn't steady — trials
  disagreeing, a canary producing nothing, the wrong model answering — is
  refused with the reason stated. Refused runs stay on disk;
  `svrn mesh bench --history` shows every run, including the ones that don't
  count and why.
- **No guessed link costs.** If we can't classify the link the split would
  ride, we say "not measured" instead of quoting a number taken over a
  different one.

If you own the machines but haven't pooled them yet,
`svrn mesh plan the-model.gguf --devices 64,32,32` (host last) sizes the
cluster you're considering — fit and split only; it will not invent a speed
for hardware it has never met.
