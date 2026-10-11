#!/usr/bin/env python3
"""Builds the GVC end-to-end fixture ladder_gvc.py is tested on, from the baseline's gold.json and GVC's raw files.

    build.py [--gold GOLD_JSON] [--corpus ~/.svrnmesh/bench-corpora/gvc]

Three dev documents (two reports of one incident, A and B, and one report the run never read, C), their gold
event mentions, and a hand-made end-to-end run: ten claims as the passes reader writes them (an `anchor` of whole
lines, a document_id stamp or an evidence source_doc_id), four happening records, a debug log and a job log.
Each claim exercises one case of the alignment; test_ladder.py states what each mention must read.
"""
import argparse, json, pathlib

HERE = pathlib.Path(__file__).resolve().parent
A, B, C = "b27405f9afd055f54cdab7c68ccff13e", "5a0c56265670498621b7cd2f71e53902", "104db82506283933234d28c49929a9cc"
DEATH, WHOLE = "a death or a killing", "the incident as a whole"


def line(body, i):
    return body.split("\n")[i]


def claim(n, subject, anchor, doc=None, source=None, kind="report"):
    attrs = {"document_id": doc} if doc else {}
    return {"atom_type": "Claim", "data": {"id": f"claim-{n:04d}", "claim_kind": kind, "subject": subject,
                                           "anchor": anchor, "attributes": attrs,
                                           "evidence": [{"chunk_id": "sec_00001", "source_doc_id": source}]}}


def record(rid, kind):
    return {"atom_type": "Entity", "data": {"id": rid, "entity_type": "happening", "attributes": {"kind": kind}}}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--gold", type=pathlib.Path,
                    default=pathlib.Path.home() / ".svrnmesh/bench-corpora/baseline-3sys-20261009/gvc/statements/gold.json")
    ap.add_argument("--corpus", type=pathlib.Path, default=pathlib.Path.home() / ".svrnmesh/bench-corpora/gvc")
    a = ap.parse_args()
    gold = json.loads(a.gold.read_text())
    docs = {d["id"]: d for d in map(json.loads, (a.corpus / "raw/documents.jsonl").read_text().splitlines()) if d["id"] in (A, B, C)}
    mentions = [m for m in map(json.loads, (a.corpus / "gold/mentions.jsonl").read_text().splitlines())
                if m["doc_id"] in docs and m["mention_id"] in gold]
    corpus = HERE / "corpus"
    (corpus / "raw").mkdir(parents=True, exist_ok=True)
    (corpus / "gold").mkdir(parents=True, exist_ok=True)
    (corpus / "raw/documents.jsonl").write_text("".join(json.dumps(docs[d], ensure_ascii=False) + "\n" for d in (A, B, C)))
    (corpus / "gold/mentions.jsonl").write_text("".join(json.dumps(m, ensure_ascii=False) + "\n" for m in mentions))
    (HERE / "gold.json").write_text(json.dumps({m["mention_id"]: gold[m["mention_id"]] for m in mentions}, indent=0) + "\n")

    a_, b_ = docs[A]["body"], docs[B]["body"]
    atoms = [
        record("rec-death", DEATH), record("rec-fire", WHOLE), record("rec-odd", "a value no declaration names"),
        claim(1, "rec-death", line(a_, 3), doc=A),                                   # one line
        claim(2, "rec-death", line(b_, 5) + "\n" + line(b_, 6), source=B),           # two lines, keyed by evidence
        claim(3, "rec-fire", line(a_, 4).replace(" the ", "  the\n"), doc=A),          # whitespace differs: folded find
        claim(4, "rec-fire", line(a_, 1), doc=A),
        claim(5, "rec-fire", line(b_, 0), doc=B),
        claim(6, "rec-death", "Police say", doc=A),                                  # twice in A: ambiguous
        claim(7, "rec-death", "a sentence no document holds", doc=A),                # absent
        claim(8, "rec-death", line(a_, 3), doc="not-a-gvc-document"),                # names no gold document
        claim(9, "entity-not-in-atlas", line(a_, 3), doc=A),                         # about no record
        claim(10, "rec-odd", "later identified as 32-year - old Chet Hockman", doc=B),  # cites line 4, whose "shot" it covers
    ]
    run = HERE / "run"
    (run / "data/indexes/cdcr-gvc/atlas").mkdir(parents=True, exist_ok=True)
    (run / "data/indexes/cdcr-gvc/atlas/atoms.json").write_text(json.dumps({"atoms": atoms}, indent=1, ensure_ascii=False) + "\n")
    ts = "2026-10-09T00:00:00.000Z [extract:err] 2026-10-09T00:00:00.000000Z DEBUG"
    dbg = [f'{ts} document_read/passes: located document={d} line=1 kind="report" p=0.9 dist=A 0.90, 0 0.10 text=x'
           for d in (A, B)]
    for phase, n in (("document_passes_locate", 18), ("document_passes_choose", 6), ("resolve_read", 4),
                     ("resolve_select", 4), ("phase_nobody_registered", 2)):
        dbg += [f"{ts} sovereign_inference: POST /v1/chat/completions ok phase={phase} model=m elapsed_ms=10"] * n
    (run / "job.debug.log").write_text("\n".join(dbg) + "\n")
    (run / "job.log").write_text("chapter read chapter=sec_00001 documents=2 claims=10 calls=34\n")
    print(json.dumps({"documents": len(docs), "mentions": len(mentions), "claims": sum(x["atom_type"] == "Claim" for x in atoms)}))


if __name__ == "__main__":
    main()
