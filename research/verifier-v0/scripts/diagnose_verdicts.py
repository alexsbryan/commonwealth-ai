#!/usr/bin/env python3
"""DIAGNOSTIC re-read of a scored run. NOT a score — do not report it as one.

    diagnose_verdicts.py <run-dir> [<run-dir> ...]

WHAT IT DOES. Pulls the verdict token out of a response with a deliberately
permissive regex — any bare GROUNDED / HALLUCINATED_INTRINSIC /
HALLUCINATED_EXTRINSIC anywhere in the text — and recomputes balanced accuracy
from it.

WHY IT EXISTS. On 2026-08-03 the first genuinely-trained checkpoint (arm A at
118 steps) scored 0.05 macro BAcc with 2,186 of 2,200 parse failures. It had not
collapsed: it was emitting correct verdicts and sound justifications inside
MALFORMED markup — opening `<answer>` and closing `</classification>`, with no
opening `<classification>` for the tolerant parser's regex to match. The harness
was right to reject it and the number was right; but "the model is wrong" and
"the model's markup is wrong" are different diagnoses with different fixes, and
nothing distinguished them.

WHY IT IS NOT THE HEADLINE. A verifier that cannot emit parseable output is not
usable, so the harness score is the one that counts for shipping. This answers a
narrower question — "is the training working?" — which the harness score cannot
answer while format and judgment fail together.

USE IT ON BOTH ARMS OF ANY COMPARISON. Applying a permissive read to one model
and the harness's parser to another manufactures a difference out of parser
strictness. Every number this prints comes from the same regex.
"""
import collections
import json
import pathlib
import re
import statistics
import sys

CLS = re.compile(r"\b(GROUNDED|HALLUCINATED_INTRINSIC|HALLUCINATED_EXTRINSIC)\b")


def diagnose(run: pathlib.Path) -> tuple[dict, int, int]:
    results = {}
    with open(run / "results.jsonl") as fh:
        for line in fh:
            r = json.loads(line)
            results[r["id"]] = r
    responses = {}
    with open(run / "responses.jsonl") as fh:
        for line in fh:
            r = json.loads(line)
            responses[r["id"]] = r.get("text", "")

    by = collections.defaultdict(collections.Counter)
    found = 0
    for rid, row in results.items():
        m = CLS.search(responses.get(rid, ""))
        if not m:
            continue
        found += 1
        pred = 1 if m.group(1) == "GROUNDED" else 0
        d = by[row["subset"]]
        if row["label"] == 1:
            d["tp" if pred else "fn"] += 1
        else:
            d["tn" if not pred else "fp"] += 1

    out = {}
    for subset, d in by.items():
        tpr = 100 * d["tp"] / max(d["tp"] + d["fn"], 1)
        tnr = 100 * d["tn"] / max(d["tn"] + d["fp"], 1)
        out[subset] = ((tpr + tnr) / 2, tpr, tnr, sum(d.values()))
    return out, found, len(results)


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        sys.exit(__doc__)
    runs = [pathlib.Path(a) for a in argv[1:]]
    scored = []
    for run in runs:
        try:
            scored.append((run.name, *diagnose(run)))
        except FileNotFoundError as exc:
            print(f"{run}: {exc}", file=sys.stderr)
            return 2

    for name, _, found, total in scored:
        print(f"{name:<16} verdict recoverable {found}/{total} "
              f"({100 * found / max(total, 1):.1f}%)")
    print("\n(DIAGNOSTIC — permissive regex, identical for every run above. "
          "Not comparable to harness scores or to BASELINES.md.)\n")

    subsets = sorted(set().union(*(set(s[1]) for s in scored)))
    names = [s[0] for s in scored]
    print(f"{'subset':<22}" + "".join(f"{n[:10]:>11}" for n in names)
          + (f"{'delta':>9}" if len(names) == 2 else ""))
    for subset in subsets:
        row = f"{subset:<22}"
        vals = []
        for _, tbl, _, _ in scored:
            v = tbl.get(subset)
            vals.append(v[0] if v else float("nan"))
            row += f"{v[0]:>11.2f}" if v else f"{'-':>11}"
        if len(vals) == 2:
            row += f"{vals[1] - vals[0]:>+9.2f}"
        print(row)

    row = f"{'MACRO':<22}"
    macros = [statistics.mean(v[0] for v in tbl.values()) for _, tbl, _, _ in scored]
    for m in macros:
        row += f"{m:>11.2f}"
    if len(macros) == 2:
        row += f"{macros[1] - macros[0]:>+9.2f}"
    print(row)

    row = f"{'mean tpr_supported':<22}"
    for _, tbl, _, _ in scored:
        row += f"{statistics.mean(v[1] for v in tbl.values()):>11.1f}"
    print(row)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
