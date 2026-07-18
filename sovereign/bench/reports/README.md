# Model-attributed reliability reports

This directory is the **durable, git-tracked, per-model** view of
Commonwealth's reliability gates. It is generated, not hand-edited.

## Why it exists

The per-lane baselines under `sovereign/bench/<group>/baselines/<id>/latest.json`
are keyed by *suite* and — historically — recorded only the slot alias
(`primary`) the run hit, not the concrete model. That made them useless
for the question a user or a release actually asks: *"what do we know
about the reliability of **this** model?"*

Two changes fixed that (see `SYSTEM_OVERVIEW.md` → Benchmarks):

1. **Capture** now resolves the alias to the concrete GGUF at run time
   (via the daemon's `/v1/models` `owned_by: "alias→<stem>"`) and stamps
   the concrete stem into every transcript row, so each baseline records
   `model_attribution` — `file_stem`, `base_name`, `family`, `quant`.
2. **This rollup** (`svrn bench report`) inverts the suite-keyed index
   into a model-keyed one.

## Layout

```
reports/
  index.json              # every model we have results for + coverage
  <model-key>/
    reliability.json      # machine-readable rollup (desktop reads this)
    REPORT.md             # human-readable card
```

`<model-key>` is the manifest `base_name` when known (so every
quantisation of the same weights clusters under one heading), else the
concrete file stem. Within a model, **each quantisation is a distinct
row** — `IQ4_NL` and `Q6_K_XL` do not behave identically, so a Q6 user
is never shown Q4's numbers as if they were their own.

## Regenerating

```
svrn bench report                      # scans sovereign/bench, writes reports/
svrn bench report --bench-root <dir>   # alternate root
```

The rollup is pure and deterministic given the baselines on disk. A
baseline that still records only an alias (a legacy capture that
predates resolution) is **surfaced as unattributed in `index.json`, not
folded into any model's numbers** — re-run that lane's bench to attribute
it.

## Honesty contract

- Different quants stay on different rows.
- Unattributed baselines are counted and named, never hidden.
- Gate verdicts (`competence ≥ 0.60`, `honesty ≥ 0.70`,
  `hallucination_rate ≤ 0.30`) mirror
  `sovereign/bench/chaos_monkey/manifest.toml` — the pre-registered
  contract, not a number chosen to make a model look good.
