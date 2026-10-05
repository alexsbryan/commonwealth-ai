#!/usr/bin/env python3
"""Detect spin-offs: per comment, does it speak to its thread's own issue, raise a different problem, or neither.

The local primary reads the thread's issue and one comment; `different` needs a verbatim quote code finds in the
comment, `this` is the default. Scored as detection against the gold (a comment whose case is not its thread's).

    spinread.py --fold tune
"""
import argparse
import collections
import importlib.util
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("support_score", HERE / "score.py")
SC = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(SC)
sys.path.insert(0, str(HERE.parent / "ward"))
from compose import ask, squash  # noqa: E402

SCHEMA = {"type": "object", "required": ["about", "quote"], "properties": {
    "about": {"type": "string", "enum": ["this", "different", "nothing"]}, "quote": {"type": "string"}}}
SYSTEM = ("You read one issue from a project's tracker and one comment posted in its thread. Say what the comment "
          "states something about: `this` = the issue's own problem or request (reproducing it, asking about it, "
          "proposing or reporting its fix, +1); `different` = a different problem or request the comment raises (a "
          "related bug, another feature, a separate use case that needs its own fix); `nothing` = no problem or "
          "request at all (thanks, chatter, bot noise). For `different`, copy the comment's sentence that raises it "
          "into quote, verbatim; otherwise quote is empty.")


def own_case(docs, gold):
    by = collections.defaultdict(list)
    for d in docs:
        by[str(d["thread"])].append(d)
    own = {}
    for t, ds in by.items():
        iss = [d for d in ds if d["kind"] == "issue" and d["id"] in gold]
        c = collections.Counter(gold[d["id"]] for d in ds if d["id"] in gold)
        own[t] = gold[iss[0]["id"]] if iss else (c.most_common(1)[0][0] if c else None)
    return own


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--fold", choices=["tune", "read"], default="tune")
    ap.add_argument("--cache", type=pathlib.Path, default=SC.ROOT / "cache/spinread")
    a = ap.parse_args()
    gold, _, _, _ = SC.load_gold(a.fold)
    docs = [json.loads(l) for l in (SC.ROOT / "raw/documents.jsonl").read_text().splitlines() if l.strip()]
    issue = {str(d["thread"]): d for d in docs if d["kind"] == "issue"}
    own = own_case(docs, gold)
    conf, calls = collections.Counter(), 0
    for d in docs:
        t = str(d["thread"])
        if d["kind"] != "comment" or d["id"] not in gold or t not in issue:
            continue
        user = (f"Issue #{t}: {issue[t].get('title', '')}\n{issue[t]['body'][:1500]}\n\n"
                f"Comment by {d['author']} ({d.get('author_association') or 'none'}):\n{d['body'][:3000]}")
        try:
            ans, cached = ask(a.cache, SYSTEM, user, SCHEMA)
        except ValueError:  # a malformed answer (a quote cut off): could not judge, counted, never defaulted
            conf["could not judge: unparseable answer"] += 1
            continue
        calls += not cached
        said = ans["about"]
        if said == "different" and not (ans["quote"].strip() and squash(ans["quote"])[:120] in squash(d["body"])):
            conf["different refused: quote not in the comment"] += 1
            said = "this"
        spin = gold[d["id"]] != own[t]
        conf[f"read {'different' if said == 'different' else 'not'}, gold {'spin-off' if spin else 'own'}"] += 1
    tp, fp = conf["read different, gold spin-off"], conf["read different, gold own"]
    fn = conf["read not, gold spin-off"]
    print(json.dumps({"precision": round(tp / (tp + fp), 3) if tp + fp else None,
                      "recall": round(tp / (tp + fn), 3) if tp + fn else None,
                      "new_calls": calls, "confusion": dict(conf)}, indent=1))


if __name__ == "__main__":
    main()
