#!/usr/bin/env python3
"""MiniCheck-Flan-T5-Large baseline runner for the M0 baseline table.

Replicates the exact MiniCheck flan-t5 inference path from
Liyan06/MiniCheck minicheck/inference.py:

  input   = 'predict: ' + eos_token.join([doc_chunk, claim])
  chunks  = doc token ids split every (max_model_len - 300), decoded back
  decode  = one generation step; support prob = softmax(logits[[3, 209]])[1]
            (3 = no-support token, 209 = support token)
  score   = max support prob over chunks; label = prob > 0.5

Row selection and metrics are IDENTICAL to eval_grounding.py — load_items
and bacc are imported from it, so BAcc rows are directly comparable to the
HalluGuard baselines. A classifier has no parse failures by construction.

  # must pass before any eval run — reproduces the model-card demo probs
  .venv/bin/python scripts/eval_minicheck.py --self-test

  .venv/bin/python scripts/eval_minicheck.py \
      --run-dir runs/baseline-minicheck-flant5-aggrefact --per-subset 200
"""

import argparse
import json
import os
import sys
import time

import torch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_grounding import bacc, load_items  # noqa: E402

MODEL_ID = "lytang/MiniCheck-Flan-T5-Large"
MAX_MODEL_LEN = 2048
CHUNK_SIZE = MAX_MODEL_LEN - 300
LABEL_TOKEN_IDS = [3, 209]  # [no-support, support], per upstream inference.py

# Model-card demo: ground truth for --self-test (README "Model Usage Demo").
DEMO_DOC = (
    "A group of students gather in the school library to study for their "
    "upcoming final exams."
)
DEMO_CLAIMS = [
    ("The students are preparing for an examination.", 1, 0.9805923700332642),
    ("The students are on vacation.", 0, 0.007121307775378227),
]


def pick_device(arg):
    if arg != "auto":
        return torch.device(arg)
    return torch.device("mps" if torch.backends.mps.is_available() else "cpu")


class Scorer:
    def __init__(self, device):
        from transformers import AutoModelForSeq2SeqLM, AutoTokenizer

        self.tokenizer = AutoTokenizer.from_pretrained(MODEL_ID)
        self.model = AutoModelForSeq2SeqLM.from_pretrained(
            MODEL_ID, torch_dtype=torch.float32
        ).to(device)
        self.model.eval()
        self.device = device

    def chunk_doc(self, doc):
        ids = self.tokenizer(doc, add_special_tokens=False)["input_ids"]
        if len(ids) <= CHUNK_SIZE:
            return [doc]
        return [
            self.tokenizer.decode(ids[i : i + CHUNK_SIZE], skip_special_tokens=True)
            for i in range(0, len(ids), CHUNK_SIZE)
        ]

    def build_inputs(self, doc, claim):
        eos = self.tokenizer.eos_token
        return ["predict: " + eos.join([chunk, claim]) for chunk in self.chunk_doc(doc)]

    @torch.no_grad()
    def support_probs(self, texts):
        """Support probability for each input text (one forward, one step)."""
        enc = self.tokenizer(
            texts,
            return_tensors="pt",
            padding=True,
            truncation=True,
            max_length=MAX_MODEL_LEN,
        ).to(self.device)
        out = self.model.generate(
            **enc,
            max_new_tokens=1,
            output_scores=True,
            return_dict_in_generate=True,
            do_sample=False,
        )
        logits = out.scores[0][:, torch.tensor(LABEL_TOKEN_IDS)].float().cpu()
        return torch.softmax(logits, dim=-1)[:, 1].tolist()

    def score_batch(self, items, batch_size):
        """Max-over-chunks support prob per item, batched across items."""
        flat, owner = [], []
        for idx, it in enumerate(items):
            for text in self.build_inputs(it["doc"], it["claim"]):
                flat.append(text)
                owner.append(idx)
        probs = [0.0] * len(items)
        for i in range(0, len(flat), batch_size):
            for j, p in enumerate(self.support_probs(flat[i : i + batch_size])):
                k = owner[i + j]
                probs[k] = max(probs[k], p)
        return probs


def self_test(scorer):
    ok = True
    for claim, want_label, want_prob in DEMO_CLAIMS:
        texts = scorer.build_inputs(DEMO_DOC, claim)
        prob = max(scorer.support_probs(texts))
        label = int(prob > 0.5)
        good = label == want_label and abs(prob - want_prob) < 0.02
        ok &= good
        print(
            f"self-test: prob={prob:.6f} (want {want_prob:.6f}) "
            f"label={label} (want {want_label}) -> {'OK' if good else 'MISMATCH'}"
        )
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", default="data/llm-aggrefact/test.parquet")
    ap.add_argument("--run-dir")
    ap.add_argument("--per-subset", type=int, default=200)
    ap.add_argument("--seed", type=int, default=17)
    ap.add_argument("--subsets", nargs="*")
    ap.add_argument("--batch-size", type=int, default=16)
    ap.add_argument("--device", default="auto")
    ap.add_argument("--item-batch", type=int, default=16)
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    device = pick_device(args.device)
    print(f"device={device.type} model={MODEL_ID}")
    scorer = Scorer(device)

    if args.self_test:
        ok = self_test(scorer)
        print("self-test:", "PASS" if ok else "FAIL")
        return 0 if ok else 1

    if not args.run_dir:
        print("--run-dir is required for an eval run", file=sys.stderr)
        return 2
    if not self_test(scorer):
        print("self-test FAILED — refusing to run the eval", file=sys.stderr)
        return 1

    os.makedirs(args.run_dir, exist_ok=True)
    results_path = os.path.join(args.run_dir, "results.jsonl")
    done_ids = set()
    if os.path.exists(results_path):
        with open(results_path) as f:
            for line in f:
                done_ids.add(json.loads(line)["id"])

    items = load_items(args.source, args.per_subset, args.subsets, args.seed)
    todo = [it for it in items if it["id"] not in done_ids]
    print(f"items to run: {len(todo)} (skipping {len(done_ids)} already done)")

    rows = []
    t0 = time.time()
    with open(results_path, "a") as out_f:
        for i in range(0, len(todo), args.item_batch):
            batch = todo[i : i + args.item_batch]
            probs = scorer.score_batch(batch, args.batch_size)
            for it, prob in zip(batch, probs):
                rec = {
                    "id": it["id"],
                    "subset": it["subset"],
                    "label": it["label"],
                    "pred": int(prob > 0.5),
                    "prob": round(prob, 6),
                }
                rows.append(rec)
                out_f.write(json.dumps(rec) + "\n")
            out_f.flush()
            done = len(done_ids) + len(rows)
            rate = len(rows) / max(1e-9, (time.time() - t0) / 60)
            print(f"{done}/{len(items)} | {rate:.1f} items/min", flush=True)

    with open(results_path) as f:
        rows = [json.loads(line) for line in f]
    by_subset = {}
    for r in rows:
        by_subset.setdefault(r["subset"], []).append(r)
    summary = {
        "model": MODEL_ID,
        "source": args.source,
        "per_subset": args.per_subset,
        "seed": args.seed,
        "device": device.type,
        "subsets": {ds: bacc(rs) for ds, rs in sorted(by_subset.items())},
    }
    summary["macro_avg_bacc"] = round(
        sum(v["bacc"] for v in summary["subsets"].values()) / len(summary["subsets"]), 2
    )
    with open(os.path.join(args.run_dir, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
