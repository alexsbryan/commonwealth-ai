#!/usr/bin/env python3
"""Score composed support cases against the uv-support gold (GOLD_SPEC.md).

    score.py --fold tune --baseline thread|events|comments   # zero-model baselines from the documents' own fields
    score.py --fold tune --pred clustering.json              # {document id: case id} from any composer
    score.py --fold tune --atlas uv-support                  # the built atlas: a document is in the case its claims are about

Composition is scored by `svrn bench er-score` (B³, pairwise, CEAF-e, LEA), the one scorer; a document
several gold cases share is left out and counted. The read fold is opened once, at the gate.
"""
import argparse
import collections
import json
import pathlib
import re
import subprocess
import sys
import tempfile

ROOT = pathlib.Path.home() / ".svrnmesh/bench-corpora/uv-support"
MAINTAINER = {"MEMBER", "OWNER", "COLLABORATOR"}
# GitHub's own duplicate convention, as a maintainer writes it; the target is the issue number.
DUP = re.compile(r"\b(?:duplicate of|dupe of|closing in favou?r of|closed in favou?r of|same as)\s+#(\d+)", re.I)
MARKED = re.compile(r"#(\d+) marked as duplicate of this issue")


def load_gold(fold):
    g = json.loads((ROOT / "gold/cases.json").read_text())
    cases = [c for c in g["cases"] if fold == "all" or c["fold"] == fold]
    of = collections.defaultdict(set)
    for c in cases:
        for d in c["documents"]:
            of[d].add(c["id"])
    gold = {d: next(iter(cs)) for d, cs in of.items() if len(cs) == 1}
    return gold, sum(len(cs) > 1 for cs in of.values()), set(g["none"]), len(cases)


def baseline(kind, docs):
    """thread: one case per thread. events: threads joined by marked_as_duplicate events. comments: also by a
    maintainer's 'Duplicate of #N'. Joins are union-find over thread numbers; nothing is read by a model."""
    parent = {}

    def root(t):
        parent.setdefault(t, t)
        while parent[t] != t:
            parent[t] = parent[parent[t]]
            t = parent[t]
        return t

    for d in docs:
        root(str(d["thread"]))
        joined = []
        if kind in ("events", "comments") and d.get("event_type") == "marked_as_duplicate":
            joined += MARKED.findall(d["body"])
        if kind == "comments" and d["kind"] == "comment" and d.get("author_association") in MAINTAINER:
            joined += DUP.findall(d["body"])
        for n in joined:
            parent[root(n)] = root(str(d["thread"]))
    return {d["id"]: root(str(d["thread"])) for d in docs}


def atlas_pred(corpus, claim_kind="case_state"):
    """{document id: case record} from a built atlas, as an app reads it: each claim of `claim_kind` puts the
    document its evidence landed in (the `document_id` stamp) into the record it is about. A document whose
    claims name several records goes to the one most of them name (first seen on a tie), and is counted."""
    path = pathlib.Path(corpus)
    if not path.is_file():
        path = pathlib.Path.home() / ".svrnmesh/indexes" / corpus / "atlas/atoms.json"
    atoms = json.loads(path.read_text())["atoms"]
    votes, counts = collections.defaultdict(collections.Counter), collections.Counter()
    for a in atoms:
        d = a.get("data") or {}
        if a.get("atom_type") != "Claim" or d.get("claim_kind") != claim_kind:
            continue
        counts["claims"] += 1
        doc = (d.get("attributes") or {}).get("document_id")
        if not d.get("subject"):
            counts["claims_without_subject"] += 1
        elif doc is None:
            counts["claims_unstamped"] += 1
        else:
            votes[str(doc)][d["subject"]] += 1
    counts["documents_in_several_records"] = sum(len(c) > 1 for c in votes.values())
    counts["records"] = len({r for c in votes.values() for r in c})
    return {doc: c.most_common(1)[0][0] for doc, c in votes.items()}, dict(counts)


def er_score(pred, gold):
    with tempfile.TemporaryDirectory() as tmp:
        p, g = pathlib.Path(tmp, "p.json"), pathlib.Path(tmp, "g.json")
        p.write_text(json.dumps(pred)); g.write_text(json.dumps(gold))
        r = subprocess.run(["sovereign", "bench", "er-score", str(p), str(g)], capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"er-score could not judge (exit {r.returncode}): {r.stderr.strip()}")
    return json.loads(r.stdout)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--fold", choices=["tune", "read", "all"], default="tune")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--baseline", choices=["thread", "events", "comments"])
    src.add_argument("--pred", type=pathlib.Path)
    src.add_argument("--atlas", metavar="CORPUS", help="read the corpus's built atlas (or an atoms.json path)")
    a = ap.parse_args()
    gold, ambiguous, none, n_cases = load_gold(a.fold)
    docs = [json.loads(l) for l in (ROOT / "raw/documents.jsonl").read_text().splitlines() if l.strip()]
    read = {}
    if a.baseline:
        pred = baseline(a.baseline, docs)
    elif a.atlas:
        pred, read = atlas_pred(a.atlas)
    else:
        pred = json.loads(a.pred.read_text())
    pred = {str(k): str(v) for k, v in pred.items()}
    r = er_score(pred, gold)
    fold_threads = {str(d["thread"]) for d in docs if d["id"] in gold}
    noise = sum(1 for d in docs if d["id"] in none and str(d["thread"]) in fold_threads and d["id"] in pred)
    pick = lambda m: {k: round(r[m][k], 3) for k in ("precision", "recall", "f1")}  # noqa: E731
    print(json.dumps({"fold": a.fold, "source": a.baseline or a.atlas or str(a.pred), "atlas_read": read,
                      "gold_cases": n_cases,
                      "gold_documents": len(gold), "ambiguous_excluded": ambiguous,
                      "scored": r["b_cubed"]["n_aligned"], "gold_unpredicted": len(set(gold) - set(pred)),
                      "no_case_documents_placed": noise,
                      "b_cubed": pick("b_cubed"), "ceaf_e": pick("ceaf_e"), "lea": pick("lea"),
                      "pairwise": pick("pairwise")}, indent=1))


if __name__ == "__main__":
    main()
