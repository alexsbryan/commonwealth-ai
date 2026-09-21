#!/usr/bin/env python3
"""Where did the RAPTOR summaries land? Pool position, admission, tree level, and whether each is a summary at all.

    summary_placement.py --runs ../runs-essay/<slug> --book ../books/<slug>.txt [--atoms <index>/atlas/atoms.json] [--out f.json]

Reads the `full` arm's eval.json rows. `admitted` is reported as null with the
reason, never inferred: the run file does not carry it. The runtime computes it
(`project_retrieved_chunks` emits `in_prompt` / `prompt_text` / `metadata` per
chunk) and `eval_cmd::runner::RetrievedChunk` drops those fields when it copies
the daemon's `retrieved_chunks` into the run file.

`level` and `verbatim` come from the index's Summary atoms, matched to the run
file's 200-char snippets by prefix; without --atoms both are null. `verbatim` is
the share of a summary's sentences found word for word in the book: 1.0 means the
"summary" is spliced book text, not an abstraction.
"""
import argparse, json, re, sys
from pathlib import Path

norm = lambda s: re.sub(r"\s+", " ", s or "").strip()  # noqa: E731


def verbatim_share(text, book, title):
    sents = [s for s in re.split(r"(?<=[.!?])\s+", norm(text).replace(f"{title} ", "")) if len(s) > 40]
    return (sum(s[:80] in book for s in sents) / len(sents)) if sents else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", required=True)
    ap.add_argument("--book", required=True)
    ap.add_argument("--atoms")
    ap.add_argument("--arm", default="full")
    ap.add_argument("--out")
    args = ap.parse_args()
    book, title = norm(Path(args.book).read_text(encoding="utf-8")), Path(args.book).stem
    atoms = []
    if args.atoms:
        atoms = [a["data"] for a in json.loads(Path(args.atoms).read_text())["atoms"] if a.get("atom_type") == "Summary"]
    tree = {"summary_atoms": len(atoms), "by_level": {}, "verbatim_1.0": 0} if atoms else None
    for a in atoms:
        a["_verbatim"] = verbatim_share(a["text"], book, title)
        tree["by_level"][str(a["level"])] = tree["by_level"].get(str(a["level"]), 0) + 1
        tree["verbatim_1.0"] += a["_verbatim"] == 1.0
    rows = []
    for d in sorted(Path(args.runs, args.arm).glob("run-*")):
        for r in json.loads((d / "eval.json").read_text())["results"]:
            pool = r.get("retrieved") or []
            hits = [(i, c) for i, c in enumerate(pool) if c.get("source") == "raptor"]
            if not hits:
                continue
            chunks = []
            for i, c in hits:
                head = norm(c["snippet"]).rstrip("….")[:150]
                m = [a for a in atoms if norm(a["text"]).startswith(head)]
                chunks.append({"position": i, "admitted": c.get("in_prompt"), "level": m[0]["level"] if len(m) == 1 else None,
                               "chars": len(norm(m[0]["text"])) if len(m) == 1 else None, "verbatim": m[0]["_verbatim"] if len(m) == 1 else None})
            rows.append({"run": d.name, "question_id": r["question_id"], "intent": (r.get("synth") or {}).get("intent"), "pool": len(pool),
                         "summaries_appended": (r.get("atlas_walk") or {}).get("summaries_appended"), "raptor": chunks})
    doc = {"arm": args.arm, "tree": tree, "rows": rows,
           "admitted_null_because": "eval.json rows carry no in_prompt / prompt_text; RetrievedChunk (eval_cmd/runner.rs) drops them"}
    for r in rows:
        c = r["raptor"]
        print(f"{r['run']} {r['question_id']}: pool {r['pool']}, raptor at {[x['position'] for x in c]}, admitted {[x['admitted'] for x in c]}, "
              f"level {[x['level'] for x in c]}, verbatim {[x['verbatim'] for x in c]}")
    print(f"tree: {tree}")
    if args.out:
        Path(args.out).write_text(json.dumps(doc, indent=1))


if __name__ == "__main__":
    sys.exit(main())
