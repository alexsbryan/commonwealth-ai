#!/usr/bin/env python3
"""Gold mentions as statements for `svrn enrich resolve-statements`, and the gold clustering that scores them.

    statements.py ~/.svrnmesh/bench-corpora/gvc --split dev [--kind event] --out DIR

Writes DIR/statements.jsonl ({document, id, start, end}: byte offsets into the body, nothing else) and
DIR/gold.json (mention id -> chain id). Only `scored` mentions of one kind. Documents are ordered by
(created_at, sha1(salt + id)): the corpus's own clock where it has one, ties and the rest in an order
blind to topic and incident; a different --salt is the order perturbation that measures a run's noise.
A discontinuous mention is given its whole extent.
"""
import argparse, hashlib, json, pathlib


def jsonl(p):
    with open(p, encoding="utf-8") as f:
        return [json.loads(l) for l in f if l.strip()]


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("corpus", type=pathlib.Path)
    ap.add_argument("--split", required=True, choices=["train", "dev", "test"])
    ap.add_argument("--kind", default="event", choices=["event", "entity"])
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--salt", default="")
    a = ap.parse_args()
    docs = {d["id"]: d for d in jsonl(a.corpus.expanduser() / "raw/documents.jsonl") if d["split"] == a.split}
    ms = [m for m in jsonl(a.corpus.expanduser() / "gold/mentions.jsonl")
          if m["doc_id"] in docs and m.get("scored", True) and m["kind"] == a.kind]
    by_doc = {}
    for m in ms:
        by_doc.setdefault(m["doc_id"], []).append(m)
    order = sorted(by_doc, key=lambda i: (docs[i]["created_at"] or "", hashlib.sha1((a.salt + i).encode()).hexdigest()))
    a.out.mkdir(parents=True, exist_ok=True)
    with open(a.out / "statements.jsonl", "w", encoding="utf-8") as f:
        for d in order:
            body = docs[d]["body"]
            for m in sorted(by_doc[d], key=lambda m: (m["start"], m["end"])):
                f.write(json.dumps({"document": d, "id": m["mention_id"],
                                    "start": len(body[:m["start"]].encode()),
                                    "end": len(body[:m["end"]].encode())}, ensure_ascii=False) + "\n")
    gold = {m["mention_id"]: m["chain_id"] for m in ms}
    (a.out / "gold.json").write_text(json.dumps(gold, ensure_ascii=False, indent=0), encoding="utf-8")
    print(json.dumps({"documents": len(order), "statements": len(ms), "chains": len(set(gold.values())),
                      "out": str(a.out)}))


if __name__ == "__main__":
    main()
