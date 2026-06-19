# Grounding gate & chaos-bench env flags

A reference for the `SOVEREIGN_*` env vars that steer the **grounding gate**, the
**agentic evidence loop**, and their **observability** — the flags you'll set when
running `sovereign bench chaos-monkey` (the grounded-or-abstain calibration bench)
or debugging the gate on the desktop/daemon path.

The code source of truth for the gate flags is `grounding_gate_flags()` in
`sovereign-core/.../runtime/grounding/config.rs` (a registry test pins it). The
agentic-loop flags live in `runtime/evidence_loop.rs`. This doc is the human
narrative for both.

## What the gate does

After synthesis produces a draft answer, the gate decides **release vs abstain**:
it checks whether the answer's asserted value is *grounded* in the retrieved
evidence. If not (violation probability ≥ τ), it abstains rather than let a
confabulation through. This is the "grounded-or-abstain" moat. The gate is **ON by
default** on every answer-producing surface (the "Grounded Everywhere" contract).

## Where the runtime actually runs (read this first)

This trips everyone up:

- **`sovereign bench chaos-monkey ...`** runs the **runtime in-process inside the
  bench binary** (`sovereign-cli-llm`): the router, gate, value-presence, agentic
  loop, and synthesis orchestration all execute *in the bench process*. It calls
  the **daemon** only for raw model inference and corpus search. So:
  - runtime env flags (gate, citation, agentic, …) must be set **on the bench
    invocation** — that's the process that reads them;
  - the gate's `dbg()` trace (`[gate] …`, `[agentic_kq] …`) goes to the **bench's
    stderr** — capture it with `2> trace.log`, **not** `daemon.err`.
- **Desktop / `sovereign serve`** runs the runtime **in the daemon**. There the
  gate's `dbg()` reaches `~/.sovereign/logs/daemon.err` via tracing (a *detached*
  daemon discards plain stderr, so `dbg()` mirrors to `tracing::info!` with the
  default `sovereign_core::…` target, which the daemon's crate-scoped filter
  passes). Set the flags on the daemon's environment in that case.

## Flag reference

### Gate
| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_GROUNDING_GATE` | **on** | Global gate on/off. `=0`/`false` disables (naked benches, latency debugging); unset or anything else = on. |
| `SOVEREIGN_GROUNDING_GATE_<SURFACE>` | unset | Per-surface override (`=1` force on, `=0` force off). `<SURFACE>` ∈ `KNOWLEDGE_QUERY`, `DEEP_QUERY`, `ATTACHED_DOC`, `COMPLEX_TASK`, `SIMPLE_QUERY`, `REFINEMENT`, `GOVERNANCE`. Beats the global default for that surface. |
| `SOVEREIGN_GV_THRESHOLD` | `0.9` | Violation-probability threshold τ. The answer abstains when its grounding-violation probability ≥ τ. Bench-calibrated; lower = stricter (more abstention). |
| `SOVEREIGN_GATE_EXCLUDE_RAPTOR` | **on** | Exclude RAPTOR *summary* chunks from the gate's evidence view (a summary isn't verbatim source). `=0` to include them. |

### Agentic evidence loop (round-2 retrieval)
| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_AGENTIC_KQ` | **off** | Enable the round-0 → sufficiency-judge → (if insufficient) formulate sub-queries → round-2 retrieval loop on KnowledgeQuery. |
| `SOVEREIGN_AGENTIC_KQ_THRESHOLD` | `0.5` | Insufficiency probability above which round-2 formulation fires. |
| `SOVEREIGN_SUFFICIENCY_CHUNKS` | `12` | How many round-0 chunks the sufficiency judge reads. |
| `SOVEREIGN_SUFFICIENCY_CHARS` | `2000` | Chars/chunk the judge reads (raised from 600 — a smaller slice truncated deep-in-chunk answers and produced false "insufficient" verdicts). |

### Citation grounding — EXPERIMENTAL, default OFF
| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_CITATION_GROUNDING` | **off** | On entity-anchored *fact* queries, force the model to copy a verbatim supporting sentence before answering; ground by quote-existence + answer-in-quote. Recovers cases where a small model confabulates a fact that's actually in one retrieved sentence (e.g. "the Doctor" for Ossipon). **Not yet a measurable aggregate win** — the bench's fast abstain-classifier mis-scores the short citation answer format, so individual recoveries don't always land in the competence number. Leave off unless you're iterating on it. See `runtime/grounding/citation.rs`. |

### Observability
| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_AGENTIC_KQ_DEBUG` | **off** | `=1` mirrors the gate + agentic-loop `dbg()` lines (`[gate] …`, `[agentic_kq] …` — draft, extracted claim, value-presence verdict, action) to **stderr** (in-process bench) **and** `tracing::info!` (deployed daemon → `daemon.err`). This is *the* switch for seeing why a probe abstained. Named `_AGENTIC_KQ_` for historical reasons; it gates **all** grounding `dbg()`, not just the loop. |

### Bench routing
| Var | Default | Effect |
|---|---|---|
| `SOVEREIGN_DISABLE_PEER_INFERENCE` | off | `=1` keeps all inference local instead of load-balancing to mesh peers. Set it for solo bench runs so results aren't perturbed by peer availability. |

## Canonical chaos-bench invocation

```bash
SOVEREIGN_DISABLE_PEER_INFERENCE=1 \   # solo run, don't route to peers
SOVEREIGN_GROUNDING_GATE=1 \           # gate on (the thing under test)
SOVEREIGN_GV_THRESHOLD=0.9 \           # default threshold
SOVEREIGN_AGENTIC_KQ=1 \               # round-2 retrieval loop on
SOVEREIGN_AGENTIC_KQ_DEBUG=1 \         # trace gate decisions to stderr
  target/debug/sovereign-cli-llm bench chaos-monkey run \
    --bank sovereign/bench/chaos_monkey/secret_agent.toml \
    --corpus chaos-secret-agent \
    --out  target/flywheel/out.jsonl \
    --transcripts target/flywheel/tr.jsonl \
    2> target/flywheel/bench-stderr.log     # <-- gate trace lands HERE
```

Then read the per-probe gate decision:
```bash
grep -aE '\[gate\]|\[agentic_kq\]' target/flywheel/bench-stderr.log
```
Each probe's sequence shows the synthesis `draft=…`, the extracted `claim=…`, the
`value-presence:` verdict (`present`/`absent → vp=…`), and the final `action=`.

## Production defaults (what ships)

Gate **on** (all surfaces), τ = 0.9, RAPTOR excluded from gate evidence. The
agentic loop, citation grounding, and debug tracing are **off** by default — they
are opt-in for benches/sweeps/debugging, not the shipped path.
