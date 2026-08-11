# verifier-v0 — M0 execution workspace

Working directory for `sovereign/docs/specs/VERIFIER_V0.md` (train a small
Qwen3.5 grounding verifier; parent: `VERIFICATION_COMMONS.md` §4/step 8).
This dir holds the M0/M1 pipeline scripts, run manifests, and findings.
Heavy assets (models, datasets) live in the HF cache, never in the repo.

## Current landmark — rung-1000

The checkpoint every finding since 2026-08-06 refers to as **`rung-1000`** is
step 1000 of run **`m3-4b-ab-46909861`** (the M3 A+B ladder, trained on Vast
pod 46909861): a PEFT LoRA adapter — rank 32, alpha 64, 248 modules — over
base `Qwen/Qwen3.5-4B`. Measured protocol: no-think + grammar. Why it is the
landmark: `findings/BASELINES.md` (BAcc 74.73 vs HalluGuard's 62.69 on the
550-item held-out bank) and `findings/HEADROOM_STUDY.md` (vs the incumbent
gate judge).

Where the bytes are — on RuggedFox (none in git), backed up 2026-08-10 to the
private HF repo `svrnmesh/verifier-v0-m3-4b-ab` (all four rung adapters +
tokenizer/trainer state, the rung-1000 Q8 GGUF, and the pod's train/launch/steps
logs; verified against the remote file listing):

- **Adapter (source of truth, ~268 MB):**
  `~/dev/train-env/runs/m3-4b-ab-46909861/hf/checkpoint-1000/`
  This is the artifact to back up. It is the scorable half pulled via
  `cloud/pod.sh rung`; optimizer/RNG/scheduler state stayed on the pod, so the
  run is servable/evaluable but not resumable from here. Sibling rungs 500,
  1500, 2000 sit beside it.
- **Servable Q8_0 GGUF (~4.3 GB):**
  `runs/scored/rung-1000/rung-1000-q8.gguf` (gitignored). Derived, not
  precious: the fused fp16 dir was deleted after conversion because
  base+adapter reproduce it in ~12 s.
- **Base model:** HF cache, `~/.cache/huggingface/hub/models--Qwen--Qwen3.5-4B`.

Rebuild or serve it with the provenance-printing scorer (base auto-resolved
from the adapter config against the HF cache; default port 8089 — the headroom
study served it on 8090):

```
./scripts/score_checkpoint.sh rung-1000 \
  ~/dev/train-env/runs/m3-4b-ab-46909861/hf/checkpoint-1000
```

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
