#!/usr/bin/env python3
"""Decide each structural link between two issue threads: the same underlying case, or only related.

Candidates come from the documents' own structure (an issue cross-reference event, a maintainer's #N). The
local primary reads both issues and the passages that make the link, and answers `same` only with a verbatim
quote that says so; code checks the quote is in a passage shown, and `related` is the default. Kept links join
threads on top of the comments baseline; the clustering is scored by score.py.

    linkread.py --fold tune --out clustering.json
"""
import argparse
import collections
import importlib.util
import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
# support/score.py and ward/score.py share a module name: load this fold's scorer by path
_spec = importlib.util.spec_from_file_location("support_score", HERE / "score.py")
SC = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(SC)
sys.path.insert(0, str(HERE.parent / "ward"))
from compose import ask, squash  # noqa: E402

XREF = re.compile(r"cross-referenced by issue #(\d+)")
REF = re.compile(r"#(\d+)\b")
SCHEMA = {"type": "object", "required": ["verdict", "quote"], "properties": {
    "verdict": {"type": "string", "enum": ["same", "related"]}, "quote": {"type": "string"}}}
SYSTEM = ("You read two issues from one project's tracker and the passages that link them. Decide whether they are "
          "the SAME underlying problem or request (one would be closed as a duplicate of the other, or a passage says "
          "they are the same, or that one is closed in favour of or tracked in the other), or only RELATED (similar, "
          "overlapping, blocking, a follow-up, or mentioned in passing). Answer same only when a passage says so, and "
          "copy that sentence verbatim into quote; for related, quote is empty.")


def candidates(docs, threads):
    out = set()
    for d in docs:
        a = str(d["thread"])
        names = XREF.findall(d["body"]) if d.get("event_type") == "cross-referenced" else []
        if d["kind"] == "comment" and d.get("author_association") in SC.MAINTAINER:
            names += REF.findall(d["body"])
        out |= {tuple(sorted((a, o))) for o in names if o != a and a in threads and o in threads}
    return sorted(out)


def passages(by_thread, a, b, limit=3):
    """Non-event documents in either thread that name the other issue, the mention in context."""
    out = []
    for t, o in ((a, b), (b, a)):
        for d in by_thread[t]:
            body = d["body"]
            # the two forms the tracker turns into a cross-reference: "#N" and the issue's own URL
            m = re.search(rf"(#{o}\b|/(?:issues|pull)/{o}\b)", body)
            i = m.start() if m else -1
            if d["kind"] != "event" and i >= 0:
                who = f"{d['author']} ({d.get('author_association') or 'none'})"
                out.append(f"[in #{t}, {d['kind']} by {who}] " + body[max(0, i - 400): i + 400])
    return out[:limit]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--fold", choices=["tune", "read"], default="tune")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--cache", type=pathlib.Path, default=SC.ROOT / "cache/linkread")
    a = ap.parse_args()
    gold, _, _, _ = SC.load_gold(a.fold)
    docs = [json.loads(l) for l in (SC.ROOT / "raw/documents.jsonl").read_text().splitlines() if l.strip()]
    by_thread = collections.defaultdict(list)
    for d in docs:
        by_thread[str(d["thread"])].append(d)
    threads = {str(d["thread"]) for d in docs if d["id"] in gold}
    issue = {str(d["thread"]): d for d in docs if d["kind"] == "issue"}
    cases_of = collections.defaultdict(set)
    for d in docs:
        if d["id"] in gold:
            cases_of[str(d["thread"])].add(gold[d["id"]])
    kept, report, calls = [], collections.Counter(), 0
    for x, y in candidates(docs, threads):
        shown = passages(by_thread, x, y)
        head = lambda t: f"#{t}: {issue[t].get('title', '')}\n{issue[t]['body'][:1200]}" if t in issue else f"#{t}"  # noqa: E731
        user = f"Issue A\n{head(x)}\n\nIssue B\n{head(y)}\n\nPassages that link them:\n" + ("\n\n".join(shown) or "(none: only a cross-reference event)")
        ans, cached = ask(a.cache, SYSTEM, user, SCHEMA)
        calls += not cached
        same = ans["verdict"] == "same"
        if same and not (ans["quote"].strip() and squash(ans["quote"])[:120] in squash(" ".join(shown))):
            report["same refused: quote not in a passage"] += 1
            same = False
        right = bool(cases_of[x] & cases_of[y])
        report[f"read {'same' if same else 'related'}, gold {'same' if right else 'different'}"] += 1
        if same:
            kept.append((x, y))
    parent = {}

    def root(t):
        parent.setdefault(t, t)
        while parent[t] != t:
            parent[t] = parent[parent[t]]
            t = parent[t]
        return t

    base = SC.baseline("comments", docs)
    thread_root = {str(d["thread"]): base[d["id"]] for d in docs}
    for x, y in kept:
        parent[root(thread_root[x])] = root(thread_root[y])
    a.out.write_text(json.dumps({d: root(c) for d, c in base.items()}))
    tp = report["read same, gold same"]
    print(json.dumps({"candidates": sum(v for k, v in report.items() if k.startswith("read")), "kept": len(kept),
                      "kept_precision": round(tp / len(kept), 3) if kept else None,
                      "gold_same_links": tp + report["read related, gold same"], "new_calls": calls,
                      "report": dict(report)}, indent=1))


if __name__ == "__main__":
    main()
