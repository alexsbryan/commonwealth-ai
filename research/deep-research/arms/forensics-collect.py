#!/usr/bin/env python3
"""DRB forensics collector (order deep-research-t7a amendment, directive
7f0e276b — the per-task forensic record the flight must produce).

Reads a landed run root (the drb-<tid>/dr-<ts>/ or loop/seed-XX/dr-<ts>/
shape) and emits ONE JSONL row per task, deterministically, from the
artifacts only:

  manifest.json      — rounds[] (round/gaps_before/gaps_after/fetched/
                       search_calls), terminal_state, budget, sources
  verdict-set.json   — empty_rounds[] (round/reason — the T7b wire form,
                       icd.rs EmptyRoundReason.as_str), claims[]

Per-task row:
  id, terminal_state, rounds (count), evidence_rounds, stalled_rounds,
  empty_rounds [{round, reason}], loop_density (evidence_rounds/rounds),
  searches (sum search_calls), fetches (sum fetched), gaps_in (first
  round's gaps_before), gaps_out (last round's gaps_after),
  gap_trace [gaps_before -> gaps_after per round], ungrounded_claims
  (claims whose citations resolve to no fetched source), claim_count,
  honesty (1 - ungrounded_claims/claim_count), truncation_declared

Evidence round: fetched > 0 OR gaps_after < gaps_before (a round that
added evidence). Stalled round: gaps_after == gaps_before AND fetched ==
0 (no movement). Reason classes are the ICD wire values
("all-admitted-fetches-refused" etc.) — the ONLY wire form.

No LLM judge anywhere. Exit 0 iff every candidate run dir produced a row.
"""
import argparse
import json
import sys
from pathlib import Path

REASONS = {
    "all-admitted-fetches-refused": "refused",
    "all-admitted-fetches-failed": "failed",
    "mixed-refused-and-failed": "mixed",
    "no-admitted-hits": "no-admits",
}


def run_dirs(run_root: Path):
    """Yield (task_id, run_dir) for every nested dr-*/ or flat run dir."""
    for task_dir in sorted(run_root.iterdir()) if run_root.is_dir() else []:
        if not task_dir.is_dir():
            continue
        cands = sorted(task_dir.glob("dr-*/"), key=lambda p: p.stat().st_mtime)
        if cands:
            yield task_dir.name, cands[-1]
        elif (task_dir / "manifest.json").exists():
            yield task_dir.name, task_dir


def collect(run_dir: Path):
    m = json.load(open(run_dir / "manifest.json", encoding="utf-8"))
    vs_path = run_dir / "verdict-set.json"
    vs = json.load(open(vs_path, encoding="utf-8")) if vs_path.exists() else {}

    rounds = m.get("rounds", [])
    gaps_before = rounds[0]["gaps_before"] if rounds else 0
    gaps_after = rounds[-1]["gaps_after"] if rounds else 0

    evidence = [r for r in rounds if r.get("fetched", 0) > 0
                or r.get("gaps_after", 0) < r.get("gaps_before", 0)]
    stalled = [r for r in rounds if r.get("gaps_after", 0) == r.get("gaps_before", 0)
               and r.get("fetched", 0) == 0]

    empty_raw = vs.get("empty_rounds", [])
    empty = [{"round": e["round"], "reason": e["reason"]} for e in empty_raw]

    # honesty: ungrounded fraction — claims whose citations resolve to
    # no source that was actually fetched (the evidence-arbiter rule;
    # score-arms.py header journals the same definition for the banks).
    # sources has carried two shapes across ICD versions: a dict
    # {"fetched": [...], "failed": [...]} (the fetched/failed split IS
    # the classification) and a flat list of items with a fetched flag.
    raw_sources = m.get("sources", [])
    if isinstance(raw_sources, dict):
        fetched_urls = {s.get("url") for s in raw_sources.get("fetched", [])}
    else:
        fetched_urls = {s.get("url") for s in raw_sources
                        if s.get("fetched") or s.get("status") == "fetched"}
    claims = vs.get("claims", [])
    ungrounded = 0
    for c in claims:
        cites = c.get("citations") or []
        if not cites:
            continue
        if not any(cu.get("url") in fetched_urls for cu in cites):
            ungrounded += 1

    return {
        "id": run_dir.parent.name,
        "run_dir": str(run_dir),
        "terminal_state": m.get("terminal_state"),
        "truncation_declared": m.get("truncation_declared"),
        "rounds": len(rounds),
        "evidence_rounds": len(evidence),
        "stalled_rounds": [r["round"] for r in stalled],
        "empty_rounds": empty,
        "reason_classes": sorted({REASONS.get(e["reason"], e["reason"])
                                  for e in empty}),
        "loop_density": round(len(evidence) / len(rounds), 3) if rounds else None,
        "searches": sum(r.get("search_calls", 0) for r in rounds),
        "fetches": sum(r.get("fetched", 0) for r in rounds),
        "gaps_in": gaps_before,
        "gaps_out": gaps_after,
        "gap_trace": [[r["gaps_before"], r["gaps_after"]] for r in rounds],
        "claim_count": len(claims),
        "ungrounded_claims": ungrounded,
        "honesty": round(1 - ungrounded / len(claims), 3) if claims else None,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run_root", help="run root (runs-t7a, runs-t6d, ...)")
    ap.add_argument("--out", default=None, help="JSONL output path")
    args = ap.parse_args()

    root = Path(args.run_root)
    rows = []
    for tid, run_dir in run_dirs(root):
        try:
            rows.append(collect(run_dir))
        except Exception as exc:  # noqa: BLE001 — report, don't hide
            print(f"collect failed for {tid}: {exc}", file=sys.stderr)
            return 2

    if args.out:
        Path(args.out).write_text(
            "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")
    else:
        for r in rows:
            print(json.dumps(r))
    print(f"# {len(rows)} task rows", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
