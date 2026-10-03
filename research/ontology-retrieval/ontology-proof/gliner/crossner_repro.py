#!/usr/bin/env python3
"""G-T0 part 1: reproduce gliner_small-v2.1's published zero-shot F1 on CrossNER AI, then
walk toward the seam one departure at a time.

    GLINER_SCRATCH=<dir> python3 crossner_repro.py --data <CrossNER_AI test.parquet> [--json crossner_repro.json]

Data: the InstructUIE zero-shot split GLiNER's eval reads (sentence + char-span entities +
the dataset's type list = labels.json), mirrored at hf bentrevett/instruct_uie_ner
CrossNER_AI/test-00000-of-00001.parquet (431 sentences).

Published: 50.7 F1, GLiNER-S, CrossNER AI, Table 1 of arXiv 2311.08526, the table the
urchade/gliner_small-v2.1 model card shows under "Named Entity Recognition benchmark result".
The card shows the paper's GLiNER-S row; it publishes no v2.1-specific row.

Arms (all CPU, flat NER, every type of the dataset as labels unless said):
  A-official  reference PyTorch, the package's own evaluate() (whitespace words, threshold 0.5)
  A-raw       reference PyTorch, raw sentence -> inference(), exact char-span micro F1 (this
              script's scorer; must agree with A-official or the scorer is wrong)
  B-raw       the seam's ONNX graph, same as A-raw
  C-seampost  B + the seam's post-processing (threshold 0.6, dedupe by (text, label))
  D-seamlabels C with the seam's fixed labels (Person/Organization/Work/Location/Event), the
              only labels the wire can reach today; lowercased onto the gold types it names
"""
import argparse, json, os, pathlib, sys, time

import pyarrow.parquet as pq

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import seam  # noqa: E402

PUBLISHED = 50.7


def prf(gold, pred):
    tp = len(gold & pred)
    p = tp / len(pred) if pred else 0.0
    r = tp / len(gold) if gold else 0.0
    return {"tp": tp, "pred": len(pred), "gold": len(gold), "p": round(100 * p, 2), "r": round(100 * r, 2),
            "f1": round(200 * p * r / (p + r), 2) if p + r else 0.0}


def gold_set(rows):
    return {(i, e["pos"][0], e["pos"][1], e["type"].lower()) for i, r in enumerate(rows) for e in r["entities"]}


def pred_set(preds, relabel=None):
    out = set()
    for i, ms in enumerate(preds):
        for m in ms:
            lab = relabel(m["label"]) if relabel else m["label"]
            if lab:
                out.add((i, m["start"], m["end"], lab))
    return out


def official(model, rows, labels):
    from gliner.evaluation.evaluate_ner import process
    from gliner.evaluation.evaluator import BaseNEREvaluator
    # gliner 0.2.26's evaluator indexes predictions as tuples, but its decoder returns Span
    # objects (TypeError: 'Span' object is not subscriptable). Span.end is inclusive (the
    # package maps it through end_token_idx_to_text_idx[span.end], model.py:1318), the same
    # convention as the gold from process(); the shim only unpacks the object.
    orig = BaseNEREvaluator.get_predictions
    BaseNEREvaluator.get_predictions = lambda self, ents: orig(
        self, [(e.start, e.end, e.entity_type) if hasattr(e, "entity_type") else e for e in ents])
    test = []
    for r in rows:
        s = process({"sentence": r["sentence"], "entities": [{"pos": e["pos"], "type": e["type"]} for e in r["entities"]]})
        s["ner_labels"] = labels
        test.append(s)
    out, f1 = model.evaluate(test, flat_ner=True, threshold=0.5, batch_size=12)
    return {"f1": round(100 * float(f1), 2), "detail": str(out)}


def tokenizer_parity(model, texts, labels):
    """The seam encodes with its own tokenizer.json (gline-rs); the package with deberta-v3-small
    plus GLiNER's added tokens. The same prompt + words must give the same ids, or the seam
    feeds the graph other inputs than the weights were trained on."""
    from tokenizers import Tokenizer
    seam_tok = Tokenizer.from_file(str(seam.SEAM_ONNX.parent.parent / "tokenizer.json"))
    coll = model.data_collator_class(model.config, data_processor=model.data_processor, return_tokens=True,
                                     return_entities=True, return_id_to_classes=True, prepare_labels=False)
    diff, example = 0, None
    for t in texts:
        tokens, _, _ = model.prepare_inputs([t])
        batch = coll(model.prepare_base_input(tokens), entity_types=labels)
        a = batch["input_ids"][0].tolist()
        prompt = [x for lab in labels for x in ("<<ENT>>", lab)] + ["<<SEP>>"] + tokens[0]
        b = seam_tok.encode(prompt, is_pretokenized=True).ids
        if a != b:
            diff += 1
            example = example or {"package": a[:24], "seam": b[:24]}
    return {"texts": len(texts), "differing": diff, "example": example}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--json", default=str(HERE / "crossner_repro.json"))
    a = ap.parse_args()
    rows = pq.read_table(a.data).to_pylist()
    labels = [x.lower() for x in rows[0]["types"]]
    assert all([x.lower() for x in r["types"]] == labels for r in rows)
    texts = [r["sentence"] for r in rows]
    gold = gold_set(rows)
    res = {"data": a.data, "sentences": len(rows), "labels": labels, "gold_entities": len(gold),
           "published": {"f1": PUBLISHED, "source": "arXiv 2311.08526 Table 1, GLiNER-S, CrossNER AI; shown on the urchade/gliner_small-v2.1 model card"},
           "arms": {}}

    t0 = time.time()
    ref = seam.load("reference")
    res["arms"]["A-official"] = official(ref, rows, labels)
    print("A-official", res["arms"]["A-official"]["f1"], f"{time.time() - t0:.0f}s", flush=True)
    a_raw = seam.infer(ref, texts, labels, 0.5)
    res["arms"]["A-raw"] = prf(gold, pred_set(a_raw))
    print("A-raw", res["arms"]["A-raw"], flush=True)

    onnx = seam.load("seam-onnx")
    res["tokenizer_parity"] = tokenizer_parity(ref, texts[:100], labels)
    print("tokenizer parity", res["tokenizer_parity"], flush=True)
    b_raw = seam.infer(onnx, texts, labels, 0.5)
    res["arms"]["B-raw"] = prf(gold, pred_set(b_raw))
    agree = pred_set(a_raw) ^ pred_set(b_raw)
    res["arms"]["B-raw"]["spans_differing_from_A_raw"] = len(agree)
    res["arms"]["B-raw"]["onnx_vs_torch_max_abs_logit_diff"] = onnx.model.max_diff
    res["arms"]["B-raw"]["batches_compared"] = onnx.model.batches
    print("B-raw", res["arms"]["B-raw"], flush=True)

    c = seam.infer(onnx, texts, labels, seam.GLINE_THRESHOLD, windowed=True, dedupe=True)
    c = [[m for m in ms if m["score"] >= seam.DEFAULT_THRESHOLD] for ms in c]
    res["arms"]["C-seampost"] = prf(gold, pred_set(c))
    c05 = seam.infer(onnx, texts, labels, 0.5, windowed=True, dedupe=True)
    res["arms"]["C-seampost-thr0.5"] = prf(gold, pred_set(c05))
    print("C-seampost", res["arms"]["C-seampost"], "thr0.5", res["arms"]["C-seampost-thr0.5"], flush=True)

    d = seam.infer(onnx, texts, seam.DEFAULT_LABELS, seam.GLINE_THRESHOLD, windowed=True, dedupe=True)
    d = [[m for m in ms if m["score"] >= seam.DEFAULT_THRESHOLD] for ms in d]
    to_gold = lambda lab: lab.lower() if lab.lower() in labels else None  # noqa: E731
    res["arms"]["D-seamlabels"] = prf(gold, pred_set(d, to_gold))
    named = {"person", "organization", "location"}
    res["arms"]["D-seamlabels"]["gold_of_types_it_can_name"] = sum(1 for g in gold if g[3] in named)
    print("D-seamlabels", res["arms"]["D-seamlabels"], flush=True)

    f1 = res["arms"]["B-raw"]["f1"]
    res["verdict"] = {"seam_onnx_minus_published": round(f1 - PUBLISHED, 2),
                      "reference_minus_published": round(res["arms"]["A-official"]["f1"] - PUBLISHED, 2),
                      "within_2": abs(f1 - PUBLISHED) <= 2.0}
    res["seconds"] = round(time.time() - t0)
    json.dump(res, open(a.json, "w"), indent=1)
    print(json.dumps(res["verdict"]))


if __name__ == "__main__":
    main()
