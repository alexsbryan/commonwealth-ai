#!/usr/bin/env python3
"""Which atom kinds did the atlas walk actually REACH, and on how many questions?

    walk_reach.py <eval.json> [<eval.json> ...] [--json out.json]

Reads `atlas_walk.nodes` from an `svrn eval run` output — the echo the walk
writes per question — and counts node kinds. The question it answers is the one
a coverage score cannot: a factor carried by `Summary` atoms is untestable on a
question whose walk never touches one, and a board that scores such a question
reports a null that means "not reached", not "did not help".

Measured 2026-09-21 on `raptor-pilot-and-his-wife`: 10 summary nodes out of 286
across 12 essay questions, all of them on 2 questions. The cause was in the
navigation map, not the atoms — the `trajectory` row seeds `Entity` and `State`
only, and 7 of the 12 essay questions land on it.

`reached` is the count of questions with at least one node of that kind. It is
the number that decides whether a factor is testable at this n, and it is kept
apart from the raw node total, which one verbose question can dominate.
"""
import argparse
import collections
import json
import pathlib
import sys


def rows(path):
    """An `svrn eval run --output` file's rows, or a refusal saying what it got.

    REFUSES anything else, including this script's OWN `--json` output — which
    is a list of summaries, has no `atlas_walk` anywhere, and silently scored
    `0 of 1` when handed back in (2026-09-22). A reach number computed from the
    wrong file is exactly the well-formed wrong answer this tool exists to
    catch, so it may not produce one (ARCH §18.3).
    """
    d = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
    r = d["results"] if isinstance(d, dict) and "results" in d else d
    if not isinstance(r, list) or not r:
        raise SystemExit(f"{path}: no `results` array — not an `eval run --output` file")
    if not any(isinstance(x, dict) and "question_id" in x for x in r):
        raise SystemExit(
            f"{path}: rows carry no `question_id` — this is not an eval run"
            + (" (it looks like walk_reach.py's own --json output)"
               if all(isinstance(x, dict) and "questions_reaching" in x for x in r) else "")
        )
    return r


def reach_counts(eval_rows):
    """`(nodes_by_kind, questions_reaching_kind)` over one run's rows.

    The ONE implementation of "did the walk reach this kind" (ARCH §10.6).
    `essay_judge` imports it for the board rather than counting again, because
    two counts of one thing is how a board and a probe come to disagree about
    whether a factor was under test.
    """
    kinds, reached = collections.Counter(), collections.Counter()
    for r in eval_rows:
        here = collections.Counter(
            (n.get("kind") or "?").lower()
            for n in ((r.get("atlas_walk") or {}).get("nodes") or [])
        )
        kinds.update(here)
        reached.update(here.keys())
    return kinds, reached


def summarise(path):
    all_rows = rows(path)
    kinds, reached = reach_counts(all_rows)
    per_q = []
    for r in all_rows:
        walk = r.get("atlas_walk") or {}
        nodes = walk.get("nodes") or []
        here = collections.Counter((n.get("kind") or "?").lower() for n in nodes)
        per_q.append({
            "question_id": r.get("question_id"),
            "nodes": len(nodes),
            "kinds": dict(here),
            "seeds": walk.get("seeds"),
            "considered": walk.get("considered"),
            "added": walk.get("added"),
            "summaries_appended": walk.get("summaries_appended"),
        })
    return {"path": str(path), "questions": len(per_q), "node_kinds": dict(kinds),
            "questions_reaching": dict(reached), "per_question": per_q}


def render(s):
    n = s["questions"]
    print(f"\n{s['path']}  —  {n} questions")
    total = sum(s["node_kinds"].values()) or 1
    print(f"  {'kind':<16} {'nodes':>6} {'share':>7} {'questions reaching':>19}")
    for k, v in sorted(s["node_kinds"].items(), key=lambda x: -x[1]):
        print(f"  {k:<16} {v:>6} {v / total:>6.1%} {s['questions_reaching'].get(k, 0):>12} of {n}")
    # The headline, said plainly: a factor nothing reaches is not under test.
    summ = s["questions_reaching"].get("summary", 0)
    print(f"  -> Summary reached on {summ} of {n} questions"
          f"{' — NOT TESTABLE at this n' if summ < n / 2 else ''}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("runs", nargs="+")
    ap.add_argument("--json")
    args = ap.parse_args()
    out = [summarise(p) for p in args.runs]
    for s in out:
        render(s)
    if args.json:
        pathlib.Path(args.json).write_text(json.dumps(out, indent=1), encoding="utf-8")
        print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
