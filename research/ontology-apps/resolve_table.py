#!/usr/bin/env python3
"""The loop's one table (ONTOLOGY_METHOD.md §The loop): score_resolve.py rows, one line per run.

    resolve_table.py SCORE_JSON...

Columns: CoNLL, MUC, B3, CEAF-e, LEA (er-score's), within- and cross-document link F1 (with the number of
cross-document links predicted and their precision), records against gold chains, model calls per document,
the proposer's recall and total tokens. A row is labelled by its score file's name without `.score.json`.
"""
import argparse, json, pathlib


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("scores", nargs="+", type=pathlib.Path)
    a = ap.parse_args()
    print("| run | CoNLL | MUC | B3 | CEAF-e | LEA | within F1 | cross F1 (links, P) | records/gold | calls/doc | recall | tokens |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for p in a.scores:
        d = json.loads(p.read_text())
        e = d["er"]
        f = lambda x: f"{x:.3f}".lstrip("0") if x < 1 else f"{x:.3f}"  # noqa: E731
        w, x = d["within_doc_links"], d["cross_doc_links"]
        print(f"| {p.name.removesuffix('.score.json')} | {f(e['conll_f1'])} | {f(e['muc']['f1'])} | "
              f"{f(e['b_cubed']['f1'])} | {f(e['ceaf_e']['f1'])} | {f(e['lea']['f1'])} | {f(w['f1'])} | "
              f"{f(x['f1'])} ({x['n']}, {f(x['p'])}) | {d['records']}/{d['gold_chains']} | "
              f"{d['calls_per_document']:.2f} | {d['proposer_recall'] if d['proposer_recall'] is not None else '-'} | "
              f"{d['tokens'].get('total_tokens', 0)} |")


if __name__ == "__main__":
    main()
