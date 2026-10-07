#!/usr/bin/env python3
"""Score composed support cases against the uv-support gold (GOLD_SPEC.md).

    score.py --fold tune --baseline thread|events|comments   # zero-model baselines from the documents' own fields
    score.py --fold tune --pred clustering.json              # {document id: case id} from any composer
    score.py --fold tune --atlas uv-support                  # the built atlas: a document is in the case its claims are about

Composition is scored by `svrn bench er-score`, the one scorer. Standard metrics are conditional on
placement; `recovery_b_cubed` keeps missing members and placed no-case documents in its denominators.
A document several gold cases share is left out and counted. The read fold is opened once, at the gate.
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
    return gold, {d for d, cs in of.items() if len(cs) > 1}, set(g["none"]), len(cases)


def evaluation_scope(pred, gold, ambiguous, none, docs, fold):
    """Keep evaluated members and known noise; diagnose exclusions before the shared scorer sees them."""
    by_id = {str(d["id"]): d for d in docs}
    if len(by_id) != len(docs):
        raise ValueError("source documents contain duplicate ids")
    unknown = set(pred) - set(by_id)
    if unknown:
        raise ValueError(f"prediction names unknown source documents: {sorted(unknown)}")
    missing = (set(gold) | ambiguous) - set(by_id)
    if missing:
        raise ValueError(f"gold names absent source documents: {sorted(missing)}")
    threads = {str(by_id[d]["thread"]) for d in set(gold) | ambiguous}
    relevant = set(by_id) if fold == "all" else {d for d, v in by_id.items() if str(v["thread"]) in threads}
    noise = none & relevant
    unlabelled = (set(pred) & relevant) - set(gold) - ambiguous - noise
    if unlabelled:
        raise ValueError(f"gold does not label predicted documents in this fold: {sorted(unlabelled)}")
    scoped = {d: c for d, c in pred.items() if d in gold or d in noise}
    return scoped, {
        "ambiguous_predictions_excluded": len(set(pred) & ambiguous),
        "predictions_outside_fold": len(set(pred) - relevant),
        "no_case_documents": len(noise),
        "no_case_documents_placed": len(set(pred) & noise),
    }


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
    document its evidence landed in (the `document_id` stamp) into the record it is about. Partition scoring
    leaves a document naming several records unplaced and counts it; it never chooses one by vote."""
    path = pathlib.Path(corpus)
    if not path.is_file():
        path = pathlib.Path.home() / ".svrnmesh/indexes" / corpus / "atlas/atoms.json"
    atoms = json.loads(path.read_text())["atoms"]
    records = {a["data"]["id"] for a in atoms if a.get("atom_type") == "Entity"
               and (a.get("data") or {}).get("entity_type") == "case"}
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
        elif d["subject"] not in records:
            counts["claims_without_case_record"] += 1
        else:
            votes[str(doc)][d["subject"]] += 1
    counts["documents_in_several_records"] = sum(len(c) > 1 for c in votes.values())
    counts["records"] = len({r for c in votes.values() for r in c})
    return {doc: next(iter(c)) for doc, c in votes.items() if len(c) == 1}, dict(counts)


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
    ap.add_argument("--membership-kind", default="case_state", help="declared claim kind whose subject gives membership; default case_state is a state-claim proxy")
    a = ap.parse_args()
    gold, ambiguous, none, n_cases = load_gold(a.fold)
    docs = [json.loads(l) for l in (ROOT / "raw/documents.jsonl").read_text().splitlines() if l.strip()]
    read = {}
    if a.baseline:
        pred = baseline(a.baseline, docs)
    elif a.atlas:
        pred, read = atlas_pred(a.atlas, a.membership_kind)
    else:
        pred = json.loads(a.pred.read_text())
    pred = {str(k): str(v) for k, v in pred.items()}
    try:
        pred, scope = evaluation_scope(pred, gold, ambiguous, none, docs, a.fold)
    except ValueError as e:
        print(f"score could not judge: {e}", file=sys.stderr)
        return 4
    r = er_score(pred, gold)
    if "recovery_b_cubed" not in r:
        print("score could not judge: er-score lacks recovery_b_cubed; rebuild sovereign-cli-bench", file=sys.stderr)
        return 4
    pick = lambda m: {k: round(r[m][k], 3) for k in ("precision", "recall", "f1")}  # noqa: E731
    print(json.dumps({"fold": a.fold, "source": a.baseline or a.atlas or str(a.pred), "atlas_read": read,
                      "membership_kind": a.membership_kind if a.atlas else None,
                      "gold_cases": n_cases,
                      "gold_documents": len(gold), "ambiguous_excluded": len(ambiguous),
                      "scored": r["b_cubed"]["n_aligned"], "gold_unpredicted": len(set(gold) - set(pred)),
                      "membership_coverage": round(len(set(gold) & set(pred)) / len(gold), 3) if gold else None,
                      **scope,
                      "recovery_b_cubed": pick("recovery_b_cubed"),
                      "b_cubed": pick("b_cubed"), "ceaf_e": pick("ceaf_e"), "lea": pick("lea"),
                      "pairwise": pick("pairwise")}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
