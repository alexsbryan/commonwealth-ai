#!/usr/bin/env python3
"""score_arch.py — run the arch bank against the bars in PREREG_arch_probes_20260817.md.

Scores the PRODUCTION register, not an easier one: each case is gated by
the same model-free gate co-arch.py uses, and the target rule's letter is
read out of the same batched call carrying whatever else fired. A case
whose target rule the gate does not fire is a GATE MISS (bar (a)) — the
model never sees it, so no judge quality can rescue it.

  score_arch.py                      # bars (a)+(b) on the daemon default
  score_arch.py --engine fast        # same bank, named engine (bar (e))
  score_arch.py --repeat 2           # bit-stability (bar (d))
  score_arch.py --cost N             # bar (c): cost over N real fired commits

Exit 0 always — this is a measurement, and a bar that is missed is a
result to report, never a crash.
"""
from __future__ import annotations

import argparse
import importlib.util
import os
import json
import statistics
import subprocess
import sys
from pathlib import Path

GYM = Path(__file__).resolve().parent
REPO = GYM.parent.parent
BANK = Path(os.environ.get("CO_ARCH_BANK") or GYM / "arch_bank.jsonl")

spec = importlib.util.spec_from_file_location("co_arch", REPO / "scripts" / "co-arch.py")
CA = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CA)

# Bars, restated here so a run cannot quietly drift from the registration.
BAR_CATCH = 0.90        # catch on planted-B
BAR_FALSE_B = 0.05      # false-B on planted-clean
BAR_COST_MS = 2500      # median per fired commit
KILL_COST_MS = 4000


def load_bank() -> list[dict]:
    return [json.loads(l) for l in BANK.read_text().splitlines() if l.strip()]


PROFILE = None


def run_case(case: dict, engine: str | None) -> dict:
    added = [(p, l) for p, l in case["lines"]]
    files = case.get("files", {"added_files": []})
    msg = case.get("msg", f"test fixture {case['id']}")
    body = "\n".join(f"{p}: {l}" for p, l in added)
    bundle = (f"=== COMMIT MESSAGE ===\n{msg}\n\n"
              f"=== ADDED CODE LINES ({len(added)} lines) ===\n{body}")
    target = case["rule"]

    # Code-decided rules never reach the model.
    for d in PROFILE["deciders"]:
        if d["id"] == target:
            v, cites = CA.run_decider(d, added, files)
            return {"id": case["id"], "rule": target, "label": case["label"],
                    "verdict": v, "gate_fired": True, "decided_by": "code",
                    "wall_ms": 0, "cites": len(cites)}

    fired = [(r, CA.gate_rule(r, added, files)) for r in PROFILE["rules"]]
    fired = [(r, c) for r, c in fired if c]
    if target not in [r["id"] for r, _ in fired]:
        return {"id": case["id"], "rule": target, "label": case["label"],
                "verdict": None, "gate_fired": False, "decided_by": "none",
                "wall_ms": 0}

    prompt = CA.build_prompt([r for r, _ in fired], bundle)
    letters, model, tel = CA.call_daemon(prompt, len(fired), model=engine)
    idx = [r["id"] for r, _ in fired].index(target)
    return {"id": case["id"], "rule": target, "label": case["label"],
            "verdict": (letters[idx] if letters else None),
            "gate_fired": True, "decided_by": "model", "model": model,
            "wall_ms": tel.get("wall_ms"), "co_fired": len(fired),
            "letters": letters}


def score(results: list[dict]) -> dict:
    by_rule: dict[str, dict] = {}
    for r in results:
        d = by_rule.setdefault(r["rule"], {"B_total": 0, "B_caught": 0,
                                           "A_total": 0, "A_false": 0,
                                           "gate_miss_B": 0, "unjudged": 0})
        if r["label"] == "B":
            d["B_total"] += 1
            if not r["gate_fired"]:
                d["gate_miss_B"] += 1
            elif r["verdict"] == "B":
                d["B_caught"] += 1
            elif r["verdict"] is None:
                d["unjudged"] += 1
        else:
            if not r["gate_fired"]:
                continue          # gate never fired: no false-B possible
            d["A_total"] += 1
            if r["verdict"] == "B":
                d["A_false"] += 1
            elif r["verdict"] is None:
                d["unjudged"] += 1
    return by_rule


def report(by_rule: dict, engine: str, results: list[dict]) -> None:
    print(f"\n=== arch bank — engine={engine or 'daemon default'} ===")
    print(f"{'rule':18} {'catch(B)':>12} {'false-B(A)':>12} {'gate-miss':>10} {'unjudged':>9}")
    tot_b = tot_bc = tot_a = tot_af = tot_gm = 0
    for rule, d in sorted(by_rule.items()):
        catch = f"{d['B_caught']}/{d['B_total']}" if d["B_total"] else "-"
        fb = f"{d['A_false']}/{d['A_total']}" if d["A_total"] else "-"
        print(f"{rule:18} {catch:>12} {fb:>12} {d['gate_miss_B']:>10} {d['unjudged']:>9}")
        tot_b += d["B_total"]; tot_bc += d["B_caught"]
        tot_a += d["A_total"]; tot_af += d["A_false"]; tot_gm += d["gate_miss_B"]
    cr = tot_bc / tot_b if tot_b else 0.0
    fr = tot_af / tot_a if tot_a else 0.0
    print(f"{'TOTAL':18} {f'{tot_bc}/{tot_b}':>12} {f'{tot_af}/{tot_a}':>12} {tot_gm:>10}")
    print(f"\nbar (a) gate recall : {tot_b - tot_gm}/{tot_b} planted-B gated in "
          f"-> {'MET' if tot_gm == 0 else f'MISSED ({tot_gm} un-gateable)'}")
    print(f"bar (b) catch       : {cr:.3f} vs >= {BAR_CATCH}  "
          f"-> {'MET' if cr >= BAR_CATCH else 'MISSED'}")
    print(f"bar (b) false-B     : {fr:.3f} vs <= {BAR_FALSE_B}  "
          f"-> {'MET' if fr <= BAR_FALSE_B else 'MISSED'}")
    ms = [r["wall_ms"] for r in results if r.get("decided_by") == "model" and r.get("wall_ms")]
    if ms:
        print(f"bank call latency   : median {statistics.median(ms):.0f}ms "
              f"min {min(ms)}ms max {max(ms)}ms (n={len(ms)}; bank hunks are "
              f"far smaller than real commits — bar (c) is the real-commit run)")


def cost_run(n: int, engine: str | None) -> None:
    """Bar (c): cost over real fired commits, in the production path."""
    shas = subprocess.run(["git", "-C", str(REPO), "rev-list",
                           "--first-parent", "-200", "HEAD"],
                          capture_output=True, text=True).stdout.split()
    picked, rows = [], []
    for s in shas:
        if len(picked) >= n:
            break
        added, files, bundle = CA.collect(s, PROFILE["globs"])
        if not added:
            continue
        fired = [(r, CA.gate_rule(r, added, files)) for r in PROFILE["rules"]]
        fired = [(r, c) for r, c in fired if c]
        if not fired:
            continue
        picked.append(s)
        prompt = CA.build_prompt([r for r, _ in fired], bundle)
        letters, model, tel = CA.call_daemon(prompt, len(fired), model=engine)
        rows.append({"sha": s[:9], "rules": len(fired), "ms": tel.get("wall_ms"),
                     "prompt_chars": tel.get("prompt_chars"),
                     "out_chars": tel.get("out_chars"),
                     "prompt_tokens": tel.get("prompt_tokens"),
                     "completion_tokens": tel.get("completion_tokens"),
                     "letters": letters, "model": model})
        print(f"  {s[:9]}  rules={len(fired)}  {tel.get('wall_ms')}ms  "
              f"prompt={tel.get('prompt_chars')}c/{tel.get('prompt_tokens')}t  "
              f"out={tel.get('out_chars')}c/{tel.get('completion_tokens')}t  {letters}")
    ms = [r["ms"] for r in rows if r["ms"]]
    if not ms:
        print("bar (c): no fired commits measured")
        return
    med = statistics.median(ms)
    print(f"\nbar (c) cost        : median {med:.0f}ms over n={len(ms)} fired commits "
          f"(min {min(ms)} max {max(ms)}) vs <= {BAR_COST_MS}ms "
          f"-> {'MET' if med <= BAR_COST_MS else 'MISSED'}"
          f"{'  [KILL: >= 4000ms]' if med >= KILL_COST_MS else ''}")
    unjudged = sum(1 for r in rows if r["letters"] is None)
    if unjudged:
        print(f"  {unjudged}/{len(rows)} commits returned no parseable letters")


def stability(bank: list[dict], engine: str | None, repeat: int) -> None:
    """Bar (d): identical letters across repeats at temperature 0."""
    print(f"\n=== bar (d) bit-stability, {repeat} repeats ===")
    unstable = []
    for case in bank:
        vs = [run_case(case, engine)["verdict"] for _ in range(repeat)]
        if len(set(map(str, vs))) > 1:
            unstable.append((case["id"], vs))
    print(f"unstable cases: {len(unstable)}/{len(bank)} "
          f"-> {'MET' if not unstable else 'MISSED'}")
    for cid, vs in unstable[:10]:
        print(f"  {cid}: {vs}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", default=None, help="fast | primary | (default routing)")
    ap.add_argument("--repeat", type=int, default=0)
    ap.add_argument("--cost", type=int, default=0)
    ap.add_argument("--out", default=None)
    ap.add_argument("--profile", default=None, help="rule set TOML")
    a = ap.parse_args()

    global PROFILE
    try:
        PROFILE = CA.load_profile(a.profile)
    except CA.ProfileError as e:
        print(f"score_arch: profile did not load: {e}")
        return 0
    print(f"profile: {PROFILE['id']} ({len(PROFILE['rules'])} model rules, "
          f"{len(PROFILE['deciders'])} code deciders)")

    if a.cost:
        print(f"=== bar (c) cost on real commits — engine={a.engine or 'default'} ===")
        cost_run(a.cost, a.engine)
        return 0

    bank = load_bank()
    if a.repeat:
        stability(bank, a.engine, a.repeat)
        return 0

    print(f"bank: {len(bank)} cases "
          f"({sum(1 for c in bank if c['label'] == 'B')} planted-B, "
          f"{sum(1 for c in bank if c['label'] == 'A')} planted-clean)")
    results = [run_case(c, a.engine) for c in bank]
    report(score(results), a.engine, results)
    if a.out:
        Path(a.out).write_text("\n".join(json.dumps(r) for r in results))
        print(f"\nrows -> {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
