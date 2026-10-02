#!/usr/bin/env python3
"""Find the permanent loss boundary for an exact, indexed passage.

Usage: RUST_LOG=retrieval.pipeline=debug svrn eval run ... 2> trace.log
       python3 trace_first_loss.py --corpus ei7-ans --quote 'summer of 1962' \
           --question 'When did ...?' --trace trace.log

This joins the corpus's FULL chunk body to the live step trace; titles, 200-char
snippets and nullable chunk ids are deliberately not passage identities. One
question per trace is recommended. No study bank is needed to run this tool.
"""

import argparse
import hashlib
import json
import re
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-9;]*m")
EVENT = re.compile(
    r'passage identities .*?step="([^"]+)" .*?query_hash=([0-9a-f]+) '
    r'before=(\[[^]]*\]) after=(\[[^]]*\])'
)


def fingerprint(corpus, title, content):
    digest = hashlib.sha256()
    for value in (corpus, title or "", content):
        raw = value.encode("utf-8")
        digest.update(len(raw).to_bytes(8, "little"))
        digest.update(raw)
    return digest.hexdigest()[:12]


def parse_trace(text, question):
    question_hash = hashlib.sha256(question.encode()).hexdigest()[:12]
    steps = []
    for line in ANSI.sub("", text).splitlines():
        match = EVENT.search(line)
        if match and match.group(2) == question_hash:
            steps.append((match.group(1), json.loads(match.group(3)), json.loads(match.group(4))))
    return steps


def first_loss(steps, passage_hash):
    if not steps:
        return "could-not-judge: no per-step events for this question"
    if steps[0][0] != "main_retrieval_mesh" or steps[-1][0] not in ("scope_audit", "prompt_admission"):
        return "could-not-judge: incomplete retrieval pipeline trace"
    if len({name for name, _, _ in steps}) != len(steps):
        return "could-not-judge: multiple turns share this question hash"
    if any(previous[2] != current[1] for previous, current in zip(steps, steps[1:])):
        return "could-not-judge: discontinuous trace between steps"
    if passage_hash in steps[-1][2]:
        if steps[-1][0] == "prompt_admission":
            return "admitted to prompt; check per-chunk truncation and synthesis/gate"
        return "survived tracked pipeline; downstream expansion and prompt admission unobserved"
    # A chunk can be removed and later reintroduced by another injector. Only
    # the final removal explains its absence from the final retrieval pool.
    losses = [name for name, before, after in steps if passage_hash in before and passage_hash not in after]
    if losses:
        return f"lost at {losses[-1]}"
    if all(passage_hash not in before and passage_hash not in after for _, before, after in steps):
        return "not in retrieval pool; inspect main search and atlas fetch supply"
    return "could-not-judge: passage appeared but loss was not observed"


def indexed_passages(corpus, quote, index_dir, row_id):
    import lancedb

    table = lancedb.connect(str(index_dir / corpus)).open_table("chunks")
    if row_id is not None:
        rows = table.search().where(f"id = {row_id}").limit(1).to_list()
    else:
        rows = table.search(quote, query_type="fts").limit(100).to_list()
    return [
        (row["id"], fingerprint(corpus, row["title"], row["content"]))
        for row in rows
        if quote.casefold() in row["content"].casefold()
    ]


def prompt_verdict(eval_run, question, quote):
    matches = [row for row in eval_run["results"] if row["question"] == question]
    if len(matches) != 1:
        return "could-not-judge: question missing or duplicated in eval JSON"
    chunks = matches[0].get("retrieved", [])
    if not chunks or not any(c.get("in_prompt") is not None for c in chunks):
        return "could-not-judge: eval JSON has no prompt-admission record"
    if any(quote.casefold() in (c.get("prompt_text") or "").casefold() for c in chunks):
        return "quote reached the recorded prompt; investigate synthesis/gate"
    return "admitted passage but quote absent from recorded prompt; inspect per-chunk truncation"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--quote", required=True)
    parser.add_argument("--question", required=True)
    parser.add_argument("--trace", type=Path, required=True)
    parser.add_argument("--row-id", type=int, help="Disambiguate quotes occurring in multiple chunks")
    parser.add_argument("--eval-json", type=Path, help="Optional synth eval JSON: distinguish retrieval from prompt admission")
    parser.add_argument("--index-dir", type=Path, default=Path.home() / ".svrnmesh/indexes")
    args = parser.parse_args()
    passages = indexed_passages(args.corpus, args.quote, args.index_dir, args.row_id)
    if len(passages) != 1:
        print(f"could-not-judge: {len(passages)} exact indexed passages; select --row-id from {passages}")
        return 2
    row, passage_hash = passages[0]
    verdict = first_loss(parse_trace(args.trace.read_text(), args.question), passage_hash)
    if verdict.startswith("admitted to prompt") and args.eval_json is not None:
        verdict = prompt_verdict(json.loads(args.eval_json.read_text()), args.question, args.quote)
    print(f"row={row} passage={passage_hash} {verdict}")
    return 2 if verdict.startswith("could-not-judge") else 0


if __name__ == "__main__":
    raise SystemExit(main())
