#!/usr/bin/env python3
"""Compare the *recorded evidence*, not passage titles, across study-1 runs.

Chunk ids and URLs are null on this corpus, and a section title occurs on many
different passages. An equal title list cannot establish equal retrieval input.
Even equal prompt_text does not establish an equal whole model prompt or draft.
"""

import json
from collections import Counter
from pathlib import Path


PROOF = Path(__file__).resolve().parent.parent


def load(arm, run):
    directory = PROOF / "runs" / arm if arm in ("bare", "full") else PROOF / f"runs-{arm}"
    rows = json.loads((directory / f"run-{run}" / "eval.json").read_text())["results"]
    return {row["question_id"]: row for row in rows}


def fingerprint(row):
    # prompt_text is the actually admitted body; for unadmitted chunks only a
    # 200-character snippet survives in eval.json, so equality is provisional.
    return [
        (c["corpus_id"], c["title"], c.get("in_prompt"), c.get("prompt_text"), c["snippet"])
        for c in row["retrieved"]
    ]


def same(rows, project):
    return len({json.dumps(project(row), sort_keys=True) for row in rows}) == 1


def compare(arm, verbose=True):
    runs = [load(arm, n) for n in (1, 2, 3)]
    classes = Counter()
    for question in runs[0]:
        rows = [run[question] for run in runs]
        answer = same(rows, lambda r: r["synth"]["answer"])
        evidence = same(rows, fingerprint)
        walk = same(rows, lambda r: r.get("atlas_walk"))
        titles = same(rows, lambda r: [c["url"] or c["title"] for c in r["retrieved"]])
        classes[(answer, evidence, walk)] += 1
        if verbose and titles and not evidence:
            print(f"{arm}: same titles, DIFFERENT evidence: {question}")
    if verbose:
        print(f"{arm}: (answer_equal, recorded_evidence_equal, walk_equal) = {dict(classes)}")
    return classes


if __name__ == "__main__":
    import sys

    for arm in sys.argv[1:] or ("bare", "full", "grounding-only"):
        compare(arm)
