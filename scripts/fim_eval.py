#!/usr/bin/env python3
"""FIM quality + latency eval (INLINE_COMPLETION.md §7, plan F3).

Runs gym/fim/cases.jsonl against a live daemon's /v1/completions and
reports, per language × case-kind and overall:

  exact            completion == expected (byte-identical)
  normalized       whitespace-collapsed match
  first_line       completion's first line == expected's first line
                    (the ghost-text accept proxy)
  ttft / total ms  adapter-side timings from sovereign_debug
  stop_rule        histogram (what terminated completions)

Usage:
  python3 scripts/fim_eval.py [--endpoint http://127.0.0.1:9741] \
      [--cases gym/fim/cases.jsonl] [--limit N] [--kind single|multi] \
      [--json out.json]

Weight-gated like fim-smoke.sh — needs a daemon whose [models.edit]
model can serve the FIM lane, i.e. /status.inference.edit carries a
`fim_style`. An editing model without FIM markers serves next-edit only
and 503s here by design; score that lane with next_edit_gen_eval.py.
Not in the default CI gate.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
from collections import Counter, defaultdict


def norm(s: str) -> str:
    return " ".join(s.split())


def first_line(s: str) -> str:
    for line in s.splitlines():
        if line.strip():
            return line.strip()
    return ""


def run_case(endpoint: str, case: dict, timeout: float) -> dict:
    body = json.dumps({
        "prefix": case["prefix"],
        "suffix": case["suffix"],
        "path": case["path"],
        "language": case["language"],
        "debug": True,
    }).encode()
    req = urllib.request.Request(
        f"{endpoint}/v1/completions",
        data=body,
        headers={"content-type": "application/json"},
    )
    t0 = time.monotonic()
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = json.loads(resp.read())
    wall_ms = (time.monotonic() - t0) * 1000
    text = payload["choices"][0]["text"]
    dbg = payload.get("sovereign_debug", {})
    expected = case["expected"]
    return {
        "id": case["id"],
        "language": case["language"],
        "kind": case["kind"],
        "text": text,
        "exact": text == expected,
        "normalized": norm(text) == norm(expected),
        "first_line": first_line(text) == first_line(expected),
        "stop_rule": dbg.get("stop_rule", "<absent>"),
        "ttft_ms": dbg.get("timings_ms", {}).get("ttft", -1),
        "total_ms": dbg.get("timings_ms", {}).get("total", -1),
        "wall_ms": round(wall_ms, 1),
        "finish_reason": dbg.get("finish_reason", "?"),
    }


def pct(vals, p):
    vals = sorted(v for v in vals if v >= 0)
    if not vals:
        return -1
    k = max(0, min(len(vals) - 1, round(p * (len(vals) - 1))))
    return vals[k]


def report(results: list[dict]) -> None:
    groups = defaultdict(list)
    for r in results:
        groups[(r["language"], r["kind"])].append(r)
        groups[("ALL", "ALL")].append(r)
    print(f"{'group':22s} {'n':>3} {'exact':>6} {'norm':>6} {'1stln':>6} {'ttft50':>7} {'tot50':>7} {'tot95':>7}")
    for (lang, kind), rs in sorted(groups.items()):
        n = len(rs)
        e = sum(r["exact"] for r in rs) / n
        nm = sum(r["normalized"] for r in rs) / n
        fl = sum(r["first_line"] for r in rs) / n
        print(f"{lang + '/' + kind:22s} {n:>3} {e:6.2f} {nm:6.2f} {fl:6.2f} "
              f"{pct([r['ttft_ms'] for r in rs], .5):7.0f} "
              f"{pct([r['total_ms'] for r in rs], .5):7.0f} "
              f"{pct([r['total_ms'] for r in rs], .95):7.0f}")
    hist = Counter(r["stop_rule"] for r in results)
    print("\nstop_rule histogram:", dict(hist.most_common()))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--endpoint", default="http://127.0.0.1:9741")
    ap.add_argument("--cases", default="gym/fim/cases.jsonl")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--kind", choices=["single", "multi"], default=None)
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--json", default=None, help="write raw per-case results here")
    args = ap.parse_args()

    cases = [json.loads(l) for l in open(args.cases, encoding="utf-8") if l.strip()]
    if args.kind:
        cases = [c for c in cases if c["kind"] == args.kind]
    if args.limit:
        cases = cases[: args.limit]
    if not cases:
        sys.exit("no cases after filtering")

    results = []
    for i, c in enumerate(cases, 1):
        try:
            r = run_case(args.endpoint, c, args.timeout)
        except Exception as e:  # noqa: BLE001 — report and continue; one bad case shouldn't kill the bank
            r = {"id": c["id"], "language": c["language"], "kind": c["kind"],
                 "error": str(e), "exact": False, "normalized": False,
                 "first_line": False, "stop_rule": "<error>", "ttft_ms": -1,
                 "total_ms": -1, "wall_ms": -1, "finish_reason": "error"}
        results.append(r)
        mark = "✓" if r["first_line"] else "·"
        print(f"[{i:>2}/{len(cases)}] {mark} {c['language']:10s} {c['kind']:6s} "
              f"{r.get('stop_rule','?'):>18s} ttft={r.get('ttft_ms',-1):>5}ms  {c['id'][:60]}")

    print()
    report(results)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(results, fh, indent=2)
        print(f"\nraw results → {args.json}")


if __name__ == "__main__":
    main()
