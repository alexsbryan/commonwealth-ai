# Version-Stamped Trees Demo — the knowledge tier knows who wrote it

The user-facing win (T1 plan §Demonstrating value, P1.3): every RAPTOR
summary node now carries its provenance — the prompt version and the
exact model that wrote it — and one command, `enrich raptor
--refresh-stale`, rebuilds exactly the trees whose provenance is
outdated and nothing else (2026-07-31).

Before today, upgrading the summarizer prompt or swapping the model
left every existing corpus silently serving summaries written by the
*old* configuration. The only remedies were "rebuild everything"
(hours across a real corpus set) or "trust that it's probably fine"
(the thing the Trust tranche exists to eliminate).

---

## The run

A tree built before stamping existed is stale by definition:

```
$ sovereign enrich raptor chaos-secret-agent --doc-type narrative --refresh-stale

  refresh-stale: current prompt rpv-2026-07-31.1 · summarizer Qwopus3.5-4B-v3-MTP-Q8_0
  stale: the-secret-agent-a-simple-tale — prompt <unstamped>; rebuilding
  [1/1] the-secret-agent-a-simple-tale  316 chunks · 19 nodes
  documents stale (rebuilt): 1
```

Run it again — the stamps now match, and the answer costs nothing:

```
  refresh-stale: current prompt rpv-2026-07-31.1 · summarizer Qwopus3.5-4B-v3-MTP-Q8_0
  documents built:  0
  documents fresh (stamps match, skipped): 1
  documents stale (rebuilt): 0
  elapsed:          0.0s
```

Flip the prompt version const (or swap the serving model) and only the
affected trees report stale, each with the reason printed:

```
  stale: the-secret-agent-a-simple-tale — summarizer Qwopus3.5-4B-v3-MTP-Q8_0 != Qwen3.6-35B-A3B-UD-MTP-IQ4_NL; rebuilding
```

The stamps live on every node row (`prompt_version`,
`summarizer_model` in `conv_raptor_nodes` / `raptor_nodes`), so the
question "which model wrote the summary my answer just cited?" is a
one-line sqlite query, per node, forever.

## What building it caught before it shipped

The first implementation resolved the expected model from the daemon's
alias table (`/v1/models`: `primary → Qwen3.6-35B`). But summary calls
are routed per call by slot policy, and the fast lane was serving the
resident 4B — so the alias table was *aspiration* while the stamps
were *attribution*, and every run reported every tree stale and
rebuilt it forever. The fix: `--refresh-stale` now sends one tiny
probe completion through the exact routing path the builder uses and
compares stamps against the model that answered. The staleness check
had to survive the same standard it enforces: provenance means what
actually served, not what the config hoped would.

## Why a product person should care

- **"Rebuild everything" becomes "rebuild what changed."** Prompt
  tweak, model upgrade, or a batch of pre-stamping corpora — one
  command finds exactly the outdated trees, prints why each one is
  stale, and leaves fresh trees untouched (a no-op check is free;
  a 316-chunk rebuild is ~3s on the fast lane).
- **Faithfulness numbers become explainable.** The faithfulness lane
  (see `FAITHFULNESS_LANE_DEMO.md`) gates each corpus's
  unsupported-claim rate against a baseline. When that rate moves
  after a rebuild, the stamps prove *why* — new prompt, new model —
  turning "the metric shifted" into "the metric shifted because we
  changed the summarizer, re-baseline deliberately."
- **Silent drift is now a visible state.** A corpus enriched last
  month by a retired model is no longer indistinguishable from one
  enriched today. Staleness is a queryable property, not a guess.

Cost: stamps are two columns written at build time (additive
migration, old rows read as unstamped); the staleness probe is one
~8-token completion against the local daemon. No cloud calls.
