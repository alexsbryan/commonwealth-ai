#!/usr/bin/env python3
"""Convert Stream A (HalluGuard-76k) and Stream B into mlx-lm-lora ORPO format.

mlx_lm_lora's ORPODataset (no-system path) expects flat strings:
  {"prompt": str, "chosen": str, "rejected": str}
and builds [{user: prompt}, {assistant: chosen|rejected}] via the tokenizer's
chat template itself. The 76k rows carry single-message lists, so we flatten.

Stream B (our harness) already emits bare-string chosen/rejected, but its
`prompt` is a DICT. Stream A's prompt content is a JSON STRING of the same
shape, so B is flattened with json.dumps to land in exactly A's register —
container unified, content untouched (spec §3: "its prompt format is theirs,
not ours" for A; B is what teaches the deployment interface).

Outputs (deterministic, seed fixed):
  data/orpo-76k/{train,valid,test}.jsonl    A only (spec §7 M1)
  data/orpo-probe/{train,valid,test}.jsonl  M0 probe subset
  data/orpo-ab/{train,valid,test}.jsonl     A+B, only with --stream-b (§7 M3)
Each dir gets a manifest.json with source sha256 + counts (run-manifest rule).
"""

import argparse
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


def _default_report() -> str:
    """Newest Stream A contamination report, preferring the post-fix one.

    `contamination_report.json` predates the 2026-07-31 claim-path counting fix
    (note 72b3ab47) — its top line counted evidence collisions only. Prefer the
    refixed report when present so decontamination is driven by the corrected
    numbers.
    """
    for name in ("contamination_report_streamA_refixed.json", "contamination_report.json"):
        p = os.path.join(ROOT, "findings", name)
        if os.path.exists(p):
            return p
    return ""


def _row_id(collision: dict) -> int:
    """Row index out of a collision example, across both report generations.

    The key was renamed `train_row` -> `row` when contamination_pass.py was
    generalized to Stream B (2026-07-31). Reading only `train_row` made this
    script KeyError against every freshly generated report while silently
    working against the stale one.
    """
    for k in ("row", "train_row"):
        if k in collision:
            return int(collision[k])
    raise KeyError(f"collision example has no row key: {sorted(collision)}")


def load_stream_b(path: str) -> list:
    """Stream B pairs -> flat ORPO strings, matching Stream A's register."""
    out = []
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        d = json.loads(line)
        prompt = d["prompt"]
        # A's prompt content is a JSON string; B carries the dict. Match A.
        if not isinstance(prompt, str):
            prompt = json.dumps(prompt, ensure_ascii=False)
        out.append({
            "prompt": prompt,
            "chosen": d["chosen"],
            "rejected": d["rejected"],
        })
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--report", default=None,
                    help="Stream A contamination report (default: newest available)")
    ap.add_argument("--stream-b", default=None,
                    help="Stream B orpo_pairs.jsonl; enables the A+B (orpo-ab) output")
    args = ap.parse_args()

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
    report_path = args.report or _default_report()
    if report_path and os.path.exists(report_path):
        with open(report_path) as f:
            report = json.load(f)
        excluded = {_row_id(c) for c in report["collision_examples"]}
        total = sum(report["colliding_training_rows"].values())
        if len(excluded) != total:
            sys.exit(
                f"contamination report lists {len(excluded)} rows but counted {total} "
                "collisions — rerun scripts/contamination_pass.py (uncapped) first"
            )
        print(f"contamination report: {os.path.basename(report_path)} ({total} collisions)")
    else:
        sys.exit(
            "no contamination report found — run scripts/contamination_pass.py "
            "--stream halluguard first. Spec §3 requires the pass before training."
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

    def emit(dirname, splits, extra=None):
        outdir = os.path.join(ROOT, "data", dirname)
        os.makedirs(outdir, exist_ok=True)
        counts = {}
        for name, data in splits.items():
            p = os.path.join(outdir, f"{name}.jsonl")
            with open(p, "w") as f:
                for r in data:
                    f.write(json.dumps(r, ensure_ascii=False) + "\n")
            counts[name] = len(data)
        manifest = {
            "source": src,
            "source_sha256": src_sha,
            "seed": SEED,
            "counts": counts,
            "contamination_excluded_rows": dropped,
            "format": "flat prompt/chosen/rejected strings for mlx_lm_lora ORPODataset",
        }
        manifest.update(extra or {})
        with open(os.path.join(outdir, "manifest.json"), "w") as f:
            json.dump(manifest, f, indent=2)
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
    # ── A+B (spec §7 M3: "4B LoRA on A+B") ──────────────────────────────────
    if args.stream_b:
        if not os.path.exists(args.stream_b):
            sys.exit(f"--stream-b path not found: {args.stream_b}")
        b_rows = load_stream_b(args.stream_b)
        if not b_rows:
            sys.exit(f"--stream-b produced 0 rows from {args.stream_b}")
        b_sha = hashlib.sha256(open(args.stream_b, "rb").read()).hexdigest()

        ab = rows + b_rows          # `rows` is already decontaminated
        random.Random(SEED).shuffle(ab)
        ab_valid = ab[:VALID_N]
        ab_test = ab[VALID_N : VALID_N + TEST_N]
        ab_train = ab[VALID_N + TEST_N :]
        b_share = len(b_rows) / len(ab)
        emit(
            "orpo-ab",
            {"train": ab_train, "valid": ab_valid, "test": ab_test},
            extra={
                "stream_a_rows": len(rows),
                "stream_b_rows": len(b_rows),
                "stream_b_source": args.stream_b,
                "stream_b_sha256": b_sha,
                "stream_b_share": round(b_share, 4),
                "note": (
                    "Stream B prompts are json.dumps of their dict so both streams "
                    "share Stream A's register. B carries its own contamination "
                    "clearance (findings/contamination_report_sep.json + _chaos)."
                ),
            },
        )
        print(f"A+B mix: {len(rows):,} A + {len(b_rows):,} B = {len(ab):,} ({b_share:.1%} B)")

    print(f"source sha256 {src_sha}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
