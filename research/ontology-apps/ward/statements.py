#!/usr/bin/env python3
"""Gold stage-update quotes as statements for `svrn enrich resolve-statements`, and the deal each is about.

    statements.py ~/.svrnmesh/bench-corpora/enron-ward --fold tune|read --out DIR [--salt S]

Writes DIR/documents.jsonl (one message per gold file: `id` <folder>/<file>, `title` the Subject, `body` the text
after the headers as the file holds it, and the headers `date`, `from`, `to`, `cc`, `message_id`, `thread_id`),
DIR/statements.jsonl ({document, id, start, end}: byte offsets into the body) and DIR/gold.json (statement id ->
deal id). Folds are deals.py's. A quote is located with runs of whitespace (and the `>` of quoted lines) matching
any such run, since the mail is line-wrapped; a quote found nowhere, and a (file, quote) gold gives two deals,
are dropped and counted. Documents are ordered by (Date, sha1(salt + id)); a different --salt is the order
perturbation that measures a run's noise. `thread_id` is the Message-ID: the corpus carries no In-Reply-To or
References, so a message is its own thread, as the mail extractor derives it.
"""
import argparse, email.parser, email.utils, hashlib, json, pathlib, re, sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from deals import FOLDS  # noqa: E402


def locate(body, quote):
    """Character span of `quote` in `body`, whitespace runs matching any whitespace run, or None."""
    words = quote.split()
    if not words:
        return None
    m = re.search(r"(?:\s|>)+".join(map(re.escape, words)), body)
    return m.span() if m else None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("corpus", type=pathlib.Path)
    ap.add_argument("--fold", required=True, choices=sorted(FOLDS))
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--salt", default="")
    a = ap.parse_args()
    root = a.corpus.expanduser()
    docs, statements, gold = {}, [], {}
    dropped = {"not_found": 0, "ambiguous": 0}
    for folder in sorted(FOLDS[a.fold]):
        g = json.loads((root / "gold" / f"{folder}.json").read_text())
        deals_of = {}
        for s in g["stage_updates"]:
            deals_of.setdefault((s["file"], s["quote"]), set()).add(s["deal"])
        for (file, quote), deals in sorted(deals_of.items()):
            if len(deals) > 1:
                dropped["ambiguous"] += 1
                continue
            doc_id = f"{folder}/{file}"
            if doc_id not in docs:
                raw = (root / "all" / folder / file).read_bytes().decode("utf-8", errors="replace")
                head, body = re.split(r"\r?\n\r?\n", raw, maxsplit=1)
                h = email.parser.HeaderParser().parsestr(head + "\n\n")
                docs[doc_id] = {"id": doc_id, "title": h.get("Subject", ""), "body": body,
                                "date": h.get("Date"), "from": h.get("From"), "to": h.get("To"),
                                "cc": h.get("Cc"), "message_id": h.get("Message-ID"),
                                "thread_id": h.get("Message-ID")}
            body = docs[doc_id]["body"]
            span = locate(body, quote)
            if span is None:
                dropped["not_found"] += 1
                continue
            start, end = len(body[:span[0]].encode()), len(body[:span[1]].encode())
            statements.append({"document": doc_id, "start": start, "end": end, "deal": deals.pop()})
    # One statement per span: two stage updates quoting one passage for one deal are one mention.
    seen, unique = set(), []
    for s in statements:
        if (s["document"], s["start"], s["end"]) not in seen:
            seen.add((s["document"], s["start"], s["end"]))
            unique.append(s)

    def when(d):
        t = email.utils.parsedate_to_datetime(docs[d]["date"]) if docs[d]["date"] else None
        return (t.timestamp() if t else 0.0, hashlib.sha1((a.salt + d).encode()).hexdigest())

    used = {s["document"] for s in unique}
    order = sorted(used, key=when)
    a.out.mkdir(parents=True, exist_ok=True)
    with open(a.out / "documents.jsonl", "w", encoding="utf-8") as f:
        for d in order:
            f.write(json.dumps(docs[d], ensure_ascii=False) + "\n")
    with open(a.out / "statements.jsonl", "w", encoding="utf-8") as f:
        for d in order:
            mine = sorted((s for s in unique if s["document"] == d), key=lambda s: (s["start"], s["end"]))
            for k, s in enumerate(mine):
                sid = f"{d}#q{k}"
                gold[sid] = s["deal"]
                f.write(json.dumps({"document": d, "id": sid, "start": s["start"], "end": s["end"]}) + "\n")
    (a.out / "gold.json").write_text(json.dumps(gold, indent=0), encoding="utf-8")
    print(json.dumps({"documents": len(order), "statements": len(gold), "deals": len(set(gold.values())),
                      "dropped": dropped, "out": str(a.out)}))


if __name__ == "__main__":
    main()
