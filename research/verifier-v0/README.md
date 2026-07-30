# verifier-v0 — M0 execution workspace

Working directory for `sovereign/docs/specs/VERIFIER_V0.md` (train a small
Qwen3.5 grounding verifier; parent: `VERIFICATION_COMMONS.md` §4/step 8).
This dir holds the M0/M1 pipeline scripts, run manifests, and findings.
Heavy assets (models, datasets) live in the HF cache, never in the repo.

## Layout

- `scripts/` — pipeline stages, each independently runnable, stdlib-first
  - `validate_76k.py` — schema validation + stats for HalluGuard-Preferences-76k
  - `prepare_orpo_data.py` — 76k → mlx-lm-lora ORPO format (full + probe splits)
- `data/` (gitignored) — converted training splits + benchmark downloads
  - `orpo-76k/`, `orpo-probe/` — flat prompt/chosen/rejected JSONL + manifest
  - `llm-aggrefact/{test,dev}.parquet` — fetched via curl (hf CLI chokes on
    this repo's hash format); gated, access granted 2026-07-29
  - `FaithBench/` — GitHub clone
- `runs/` (gitignored) — one dir per training/eval run: config + log + adapters
- `findings/` — committed results: validation reports, license audit, eval cards
- `.venv` — uv venv: `mlx-lm-lora` + `mlx-lm` (the M2 Max lane stack per spec §4)

## Environment

```
uv venv .venv --python 3.13
uv pip install --python .venv/bin/python mlx-lm-lora mlx-lm datasets huggingface_hub
```

mlx 0.32.0 / mlx-lm 0.31.3 / Metal verified 2026-07-29. Qwen3.5 (`qwen3_5`)
is natively supported; chat template preserves `<think>` + answer XML.

## Gotchas learned so far

- `mlx_lm_lora.train -c config.yaml` only fills args whose argparse default is
  `None` — flags like `--train-mode` silently keep their CLI defaults over the
  YAML value. Pass operative flags on the CLI; use YAML only for
  `lora_parameters` (which has no flag).
- The 76k rows carry single-message chat lists; `ORPODataset`'s no-system path
  wants flat strings — `prepare_orpo_data.py` flattens.
- Responses quote the answer-format template inside `<think>` — any verdict
  parser must read only after the last `</think>` (validate_76k.py does).
- `hf download lytang/LLM-AggreFact` fails ("Unable to parse string as hex
  hash value"); direct `curl` with the bearer token works.
