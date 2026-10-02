#!/usr/bin/env python3
"""Diff two voice-eval JSON reports and print a per-axis / per-scenario
comparison. Designed for the "small model vs large model" baseline so
the next iteration of voice-contract work has a single artifact to
point at.

Usage:
  diff_report.py <small.json> <large.json>
"""
import json
import statistics
import sys
from pathlib import Path

JUDGE_AXES = [
    "right_attention",
    "right_specificity",
    "right_calibration",
    "right_question",
    "right_silence",
    "right_disagreement",
    "right_edge",
    "right_self_honesty",
]


def load(path: str) -> dict:
    return json.loads(Path(path).read_text())


def latency_stats(samples: list[int]) -> tuple[int, int, int]:
    if not samples:
        return (0, 0, 0)
    s = sorted(samples)
    n = len(s)
    median = s[n // 2]
    p95_idx = max(0, min(n - 1, int(-(-n * 0.95 // 1)) - 1))  # ceil
    return median, s[p95_idx], s[-1]


def axis_means(judge_scores: list) -> dict[str, float]:
    out = {}
    real = [j for j in judge_scores if j is not None]
    n = len(real)
    if n == 0:
        return {a: 0.0 for a in JUDGE_AXES + ["avoid_list_penalty"]}
    for a in JUDGE_AXES:
        out[a] = sum(j[a] for j in real) / n
    out["avoid_list_penalty"] = sum(j["avoid_list_penalty"] for j in real) / n
    return out


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    small = load(sys.argv[1])
    large = load(sys.argv[2])

    print(f"Small chat model: {small.get('chat_model', '?')}")
    print(f"Large chat model: {large.get('chat_model', '?')}")
    print(f"Judge model:      {small.get('judge_model', '?')}  (small)")
    print(f"Judge model:      {large.get('judge_model', '?')}  (large)")
    print()

    sa = small["aggregate"]
    la = large["aggregate"]
    print(f"{'metric':30} {'small':>10}  {'large':>10}  {'Δ':>10}")
    print("-" * 64)
    print(f"{'pass count':30} {sa['passed']:>10}  {la['passed']:>10}  {la['passed']-sa['passed']:>+10}")
    print(f"{'fail count':30} {sa['failed']:>10}  {la['failed']:>10}  {la['failed']-sa['failed']:>+10}")
    for ck in ["length", "question_density", "banned_phrases", "required_content"]:
        s = sa["by_check"][ck]
        l = la["by_check"][ck]
        print(
            f"{'  ' + ck + ' (passed/enabled)':30} "
            f"{s['passed']:>4}/{s['enabled']:<5}  "
            f"{l['passed']:>4}/{l['enabled']:<5}  "
            f"{l['passed']-s['passed']:>+10}"
        )

    sm = latency_stats(small.get("runtime_ms", []))
    lm = latency_stats(large.get("runtime_ms", []))
    print()
    print("Runtime latency (ms)")
    print(f"  median  small={sm[0]:>7}  large={lm[0]:>7}  Δ={lm[0]-sm[0]:>+7}  ({(lm[0]/sm[0]-1)*100:+.0f}%)" if sm[0] else "")
    print(f"  p95     small={sm[1]:>7}  large={lm[1]:>7}  Δ={lm[1]-sm[1]:>+7}")
    print(f"  max     small={sm[2]:>7}  large={lm[2]:>7}  Δ={lm[2]-sm[2]:>+7}")

    print()
    print("Judge axes (mean over scenarios; 0=worst, 3=best — except avoid_list_penalty where lower=better)")
    sm_axes = axis_means(small.get("judge_scores", []))
    lg_axes = axis_means(large.get("judge_scores", []))
    print(f"  {'axis':22} {'small':>6}  {'large':>6}  {'Δ':>6}")
    for a in JUDGE_AXES + ["avoid_list_penalty"]:
        s = sm_axes[a]
        l = lg_axes[a]
        marker = ""
        if a == "avoid_list_penalty":
            marker = "  (lower better)"
        print(f"  {a:22} {s:6.2f}  {l:6.2f}  {l-s:+6.2f}{marker}")

    print()
    print("Per-scenario length blowout (response_chars / max_chars)")
    print(f"  {'scenario':38} {'cap':>5}  {'small':>6}  {'large':>6}")
    by_id_s = {r["scenario_id"]: r for r in small["results"]}
    by_id_l = {r["scenario_id"]: r for r in large["results"]}
    for sid in sorted(by_id_s):
        rs = by_id_s[sid]
        rl = by_id_l.get(sid)
        cap = rs["length"]["max_chars"] or 0
        actual_s = rs["length"]["response_chars"]
        actual_l = rl["length"]["response_chars"] if rl else 0
        ratio_s = (actual_s / cap) if cap else 0
        ratio_l = (actual_l / cap) if cap else 0
        print(f"  {sid:38} {cap:>5}  {actual_s:>5} {ratio_s:>4.1f}x  {actual_l:>5} {ratio_l:>4.1f}x")

    print()
    print("Common failure mode (first 120 chars):")
    for sid in sorted(by_id_s):
        rs = by_id_s[sid]
        opener = rs["response"][:120].replace("\n", " ")
        print(f"  small  {sid:38} {opener!r}")
        rl = by_id_l.get(sid)
        if rl:
            opener = rl["response"][:120].replace("\n", " ")
            print(f"  large  {sid:38} {opener!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
