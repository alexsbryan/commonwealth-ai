#!/usr/bin/env python3
"""Convert HalluGuard-Preferences-76k into mlx-lm-lora ORPO format.

mlx_lm_lora's ORPODataset (no-system path) expects flat strings:
  {"prompt": str, "chosen": str, "rejected": str}
and builds [{user: prompt}, {assistant: chosen|rejected}] via the tokenizer's
chat template itself. The 76k rows carry single-message lists, so we flatten.

Outputs (deterministic, seed fixed):
  data/orpo-76k/{train,valid,test}.jsonl    full run (train 74,708 / valid 1,000 / test 1,000)
  data/orpo-probe/{train,valid,test}.jsonl  M0 probe subset (train 2,000 / valid 200 / test 200)
Each dir gets a manifest.json with source sha256 + counts (run-manifest rule).
"""

import glob
import hashlib
import json
import os
import random
import sys

SEED = 17
VALID_N = 1000
TEST_N = 1000
PROBE_TRAIN = 2000
PROBE_VALID = 200
PROBE_TEST = 200

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))


def main() -> int:
    pats = glob.glob(
        os.path.expanduser(
            "~/.cache/huggingface/hub/datasets--lrsbrgrn--HalluGuard-Preferences-76k/"
            "snapshots/*/halluguard-main.jsonl"
        )
    )
    if not pats:
        sys.exit("dataset jsonl not found in HF cache")
    src = pats[0]

    # Decontamination (spec section 3): drop rows the contamination pass flagged
    # as sharing a >=13-gram span with an external test set. The report must be
    # complete — a capped/partial collision list would silently under-exclude.
    excluded = set()
    report_path = os.path.join(ROOT, "findings", "contamination_report.json")
    if os.path.exists(report_path):
        with open(report_path) as f:
            report = json.load(f)
        excluded = {c["train_row"] for c in report["collision_examples"]}
        total = sum(report["colliding_training_rows"].values())
        if len(excluded) != total:
            sys.exit(
                f"contamination report lists {len(excluded)} rows but counted {total} "
                "collisions — rerun scripts/contamination_pass.py (uncapped) first"
            )

    sha = hashlib.sha256()
    rows = []
    dropped = 0
    with open(src, "rb") as f:
        for i, raw in enumerate(f):
            sha.update(raw)
            if (i + 1) in excluded:
                dropped += 1
                continue
            d = json.loads(raw)
            rows.append(
                {
                    "prompt": d["prompt"][0]["content"],
                    "chosen": d["chosen"][0]["content"],
                    "rejected": d["rejected"][0]["content"],
                }
            )
    src_sha = sha.hexdigest()
    print(f"decontamination: {dropped} rows dropped ({len(excluded)} flagged)")

    rng = random.Random(SEED)
    rng.shuffle(rows)
    valid = rows[:VALID_N]
    test = rows[VALID_N : VALID_N + TEST_N]
    train = rows[VALID_N + TEST_N :]

    def emit(dirname, splits):
        outdir = os.path.join(ROOT, "data", dirname)
        os.makedirs(outdir, exist_ok=True)
        counts = {}
        for name, data in splits.items():
            p = os.path.join(outdir, f"{name}.jsonl")
            with open(p, "w") as f:
                for r in data:
                    f.write(json.dumps(r, ensure_ascii=False) + "\n")
            counts[name] = len(data)
        with open(os.path.join(outdir, "manifest.json"), "w") as f:
            json.dump(
                {
                    "source": src,
                    "source_sha256": src_sha,
                    "seed": SEED,
                    "counts": counts,
                    "contamination_excluded_rows": dropped,
                    "format": "flat prompt/chosen/rejected strings for mlx_lm_lora ORPODataset",
                },
                f,
                indent=2,
            )
        print(f"{dirname}: {counts} -> {outdir}")

    emit("orpo-76k", {"train": train, "valid": valid, "test": test})
    emit(
        "orpo-probe",
        {
            "train": train[:PROBE_TRAIN],
            "valid": valid[:PROBE_VALID],
            "test": test[:PROBE_TEST],
        },
    )
    print(f"source sha256 {src_sha}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
