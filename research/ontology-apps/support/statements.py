#!/usr/bin/env python3
"""Gold case-state quotes as statements for `svrn enrich resolve-statements`, and the case each is about.

    statements.py ~/.svrnmesh/bench-corpora/uv-support --fold tune|read --out DIR [--salt S]

Writes DIR/statements.jsonl ({document, id, start, end}: byte offsets into the body of raw/documents.jsonl, which
is the run's --documents as it stands) and DIR/gold.json (statement id -> case id). A quote is the first
occurrence in its document's body (GOLD_SPEC: verbatim); one found nowhere, and a (document, quote) gold gives two
cases, are dropped and counted. Documents are ordered by (created_at, sha1(salt + id)); a different --salt is the
order perturbation that measures a run's noise.
"""
import argparse, hashlib, json, pathlib


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("corpus", type=pathlib.Path)
    ap.add_argument("--fold", required=True, choices=["tune", "read"])
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--salt", default="")
    a = ap.parse_args()
    root = a.corpus.expanduser()
    with open(root / "raw/documents.jsonl", encoding="utf-8") as f:
        docs = {d["id"]: d for d in map(json.loads, filter(str.strip, f))}
    g = json.loads((root / "gold/cases.json").read_text())
    fold_of = {c["id"]: c["fold"] for c in g["cases"]}
    cases_of = {}
    for s in g["case_states"]:
        if fold_of[s["case"]] == a.fold:
            cases_of.setdefault((s["document"], s["quote"]), set()).add(s["case"])
    dropped = {"not_found": 0, "ambiguous": 0}
    spans = {}
    for (d, quote), cases in cases_of.items():
        if len(cases) > 1:
            dropped["ambiguous"] += 1
            continue
        body = docs[d]["body"] or ""
        at = body.find(quote) if quote else -1
        if at < 0:
            dropped["not_found"] += 1
            continue
        start = len(body[:at].encode())
        spans.setdefault((d, start, start + len(quote.encode())), cases.pop())
    order = sorted({d for d, _, _ in spans},
                   key=lambda d: (docs[d]["created_at"] or "", hashlib.sha1((a.salt + d).encode()).hexdigest()))
    a.out.mkdir(parents=True, exist_ok=True)
    gold = {}
    with open(a.out / "statements.jsonl", "w", encoding="utf-8") as f:
        for d in order:
            for k, (_, start, end) in enumerate(sorted(s for s in spans if s[0] == d)):
                sid = f"{d}#q{k}"
                gold[sid] = spans[(d, start, end)]
                f.write(json.dumps({"document": d, "id": sid, "start": start, "end": end}) + "\n")
    (a.out / "gold.json").write_text(json.dumps(gold, indent=0), encoding="utf-8")
    print(json.dumps({"documents": len(order), "statements": len(gold), "cases": len(set(gold.values())),
                      "dropped": dropped, "out": str(a.out)}))


if __name__ == "__main__":
    main()
