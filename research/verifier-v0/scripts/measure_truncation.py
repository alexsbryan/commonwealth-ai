#!/usr/bin/env python3
"""Token-length distribution of the ORPO splits, per stream.

Answers the question MAC_MIGRATION.md §4 defers to "before M1": at
max_seq_length 4096 and max_prompt_length 2048, how many rows lose bytes?

This matters more here than in a generic SFT run. The prompt IS the evidence
document; the label is "is this claim grounded in that document". Truncate the
document and the row teaches the model to answer a question whose answer was
cut off — a label the model cannot verify. Over-truncation in one stream and
not the other also silently reweights the mix study.

Reports per stream so A and B are comparable, since the whole point of the M2
mix study is whether B's register behaves like A's.

Usage:
    .venv/bin/python scripts/measure_truncation.py [--sample N] [--seq 4096]
"""
import argparse
import json
import os
import random
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))

# Streams to measure, as (label, path, prompt_key). Stream B is read from its
# own pairs file rather than out of orpo-ab, because orpo-ab drops the meta
# that says which stream a row came from.
STREAMS = [
    ("A (orpo-76k train)", "data/orpo-76k/train.jsonl"),
    ("B (stream_b pairs)", "data/stream_b/all/orpo_pairs.jsonl"),
]


def pct(xs, p):
    if not xs:
        return 0
    xs = sorted(xs)
    k = min(len(xs) - 1, int(round((p / 100.0) * (len(xs) - 1))))
    return xs[k]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="Qwen/Qwen3.5-0.8B")
    ap.add_argument("--sample", type=int, default=6000,
                    help="rows per stream (0 = all); sampling is seeded")
    ap.add_argument("--seq", type=int, default=4096, help="max_seq_length")
    ap.add_argument("--max-prompt", type=int, default=2048, help="max_prompt_length")
    ap.add_argument("--seed", type=int, default=17)
    ap.add_argument("--json-out", default="findings/truncation_report.json")
    args = ap.parse_args()

    from transformers import AutoTokenizer

    tok = AutoTokenizer.from_pretrained(args.model)
    report = {
        "model": args.model,
        "max_seq_length": args.seq,
        "max_prompt_length": args.max_prompt,
        "sample_per_stream": args.sample or "all",
        "seed": args.seed,
        "streams": {},
    }

    for label, rel in STREAMS:
        path = os.path.join(ROOT, rel)
        if not os.path.exists(path):
            print(f"SKIP {label}: {rel} not found", file=sys.stderr)
            continue

        with open(path) as f:
            lines = f.readlines()
        n_total = len(lines)
        if args.sample and args.sample < n_total:
            lines = random.Random(args.seed).sample(lines, args.sample)

        prompts, wholes = [], []
        for line in lines:
            d = json.loads(line)
            p = d["prompt"]
            if not isinstance(p, str):
                p = json.dumps(p, ensure_ascii=False)
            np_ = len(tok(p, add_special_tokens=False)["input_ids"])
            # ORPO packs prompt + the longer of the two completions.
            nc = max(
                len(tok(str(d["chosen"]), add_special_tokens=False)["input_ids"]),
                len(tok(str(d["rejected"]), add_special_tokens=False)["input_ids"]),
            )
            prompts.append(np_)
            wholes.append(np_ + nc)

        n = len(prompts)
        over_prompt = sum(1 for x in prompts if x > args.max_prompt)
        over_seq = sum(1 for x in wholes if x > args.seq)
        report["streams"][label] = {
            "rows_total": n_total,
            "rows_measured": n,
            "prompt_tokens": {
                "p50": pct(prompts, 50), "p90": pct(prompts, 90),
                "p99": pct(prompts, 99), "max": max(prompts),
            },
            "prompt_plus_completion_tokens": {
                "p50": pct(wholes, 50), "p90": pct(wholes, 90),
                "p99": pct(wholes, 99), "max": max(wholes),
            },
            "over_max_prompt": over_prompt,
            "over_max_prompt_pct": round(100.0 * over_prompt / n, 3),
            "over_max_seq": over_seq,
            "over_max_seq_pct": round(100.0 * over_seq / n, 3),
        }

        s = report["streams"][label]
        print(f"\n{label}   ({n:,} of {n_total:,} rows measured)")
        print("  prompt tokens        p50 %-6d p90 %-6d p99 %-6d max %d"
              % (s["prompt_tokens"]["p50"], s["prompt_tokens"]["p90"],
                 s["prompt_tokens"]["p99"], s["prompt_tokens"]["max"]))
        print("  prompt+completion    p50 %-6d p90 %-6d p99 %-6d max %d"
              % (s["prompt_plus_completion_tokens"]["p50"],
                 s["prompt_plus_completion_tokens"]["p90"],
                 s["prompt_plus_completion_tokens"]["p99"],
                 s["prompt_plus_completion_tokens"]["max"]))
        print("  > max_prompt %-5d    %d rows (%.2f%%)"
              % (args.max_prompt, over_prompt, s["over_max_prompt_pct"]))
        print("  > max_seq    %-5d    %d rows (%.2f%%)"
              % (args.seq, over_seq, s["over_max_seq_pct"]))

    out = os.path.join(ROOT, args.json_out)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print(f"\nreport -> {args.json_out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
