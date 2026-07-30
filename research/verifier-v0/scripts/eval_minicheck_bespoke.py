#!/usr/bin/env python3
"""Bespoke-MiniCheck-7B baseline runner for the M0 baseline table.

Replicates the upstream MiniCheck LLM path (Liyan06/MiniCheck) with plain
transformers on MPS — vLLM (the upstream engine for this variant) does not
exist on macOS:

  messages = [system: SYSTEM_PROMPT, user: 'Document: {doc}\nClaim: {claim}']
  text     = tokenizer.apply_chat_template(messages, add_generation_prompt=True)
  decode   = single forward pass; support prob = sum of softmax mass over all
             vocab tokens whose decoded form lowercases to 'yes'
             (upstream sums exp(logprob) over returned logprobs the same way)
  label    = prob > 0.5

No doc chunking: upstream chunks only past ~32K tokens and every benchmark
doc here is far under. Claims are passed wholesale (same treatment as our
flan-t5 row). Sampling and metrics come from eval_grounding.load_items/bacc,
identical to every other baseline row.

  .venv/bin/python scripts/eval_minicheck_bespoke.py --self-test
  .venv/bin/python scripts/eval_minicheck_bespoke.py \
      --run-dir runs/baseline-bespoke7b-aggrefact --per-subset 200
"""

import argparse
import json
import os
import sys
import time

import torch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_grounding import bacc, load_items  # noqa: E402

MODEL_ID = "bespokelabs/Bespoke-MiniCheck-7B"

SYSTEM_PROMPT = (
    "Determine whether the provided claim is consistent with the corresponding "
    "document. Consistency in this context implies that all information "
    "presented in the claim is substantiated by the document. If not, it "
    "should be considered inconsistent. Please assess the claim's consistency "
    'with the document by responding with either "Yes" or "No".'
)
USER_PROMPT = "Document: [DOCUMENT]\nClaim: [CLAIM]"

# Model-card demo: ground truth for --self-test.
DEMO_DOC = (
    "A group of students gather in the school library to study for their "
    "upcoming final exams."
)
DEMO_CLAIMS = [
    ("The students are preparing for an examination.", 1, 0.9840446675150499),
    ("The students are on vacation.", 0, 0.010986349594852094),
]


def pick_device(arg):
    if arg != "auto":
        return torch.device(arg)
    return torch.device("mps" if torch.backends.mps.is_available() else "cpu")


class Scorer:
    def __init__(self, device, dtype):
        from transformers import AutoModelForCausalLM, AutoTokenizer

        self.tokenizer = AutoTokenizer.from_pretrained(
            MODEL_ID, trust_remote_code=True, padding_side="left"
        )
        # sdpa is mandatory on MPS: the remote code's eager attention
        # materializes the full N^2 matrix and segfaults on ~6k-token docs
        # (observed exit=139 at 64/2200); docs here run up to 21k tokens.
        self.model = AutoModelForCausalLM.from_pretrained(
            MODEL_ID, trust_remote_code=True, torch_dtype=dtype,
            attn_implementation="sdpa",
        ).to(device)
        self.model.eval()
        self.device = device
        self.yes_ids = self._yes_token_ids()
        print(f"yes-token ids: {self.yes_ids}")

    def _yes_token_ids(self):
        """All vocab ids whose decoded form lowercases to 'yes' — the causal-LM
        equivalent of upstream's decoded_token.lower() == 'yes' logprob sum."""
        ids = []
        for tok, idx in self.tokenizer.get_vocab().items():
            if tok.lower().replace("▁", "").replace("Ġ", "") == "yes":
                ids.append(idx)
        return sorted(ids)

    def build_text(self, doc, claim):
        user = USER_PROMPT.replace("[DOCUMENT]", doc).replace("[CLAIM]", claim)
        messages = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user},
        ]
        return self.tokenizer.apply_chat_template(
            messages, add_generation_prompt=True, tokenize=False
        )

    @torch.no_grad()
    def support_probs(self, pairs):
        texts = [self.build_text(doc, claim) for doc, claim in pairs]
        enc = self.tokenizer(texts, return_tensors="pt", padding=True).to(self.device)
        # use_cache=False skips the transformers-4.x DynamicCache.from_legacy_cache
        # path in the model's remote code, which no longer exists in 5.x — and a
        # single scoring forward has no use for KV cache anyway.
        logits = self.model(**enc, use_cache=False).logits[:, -1, :].float().cpu()
        probs = torch.softmax(logits, dim=-1)
        return probs[:, self.yes_ids].sum(dim=-1).tolist()


def self_test(scorer):
    ok = True
    for claim, want_label, want_prob in DEMO_CLAIMS:
        prob = scorer.support_probs([(DEMO_DOC, claim)])[0]
        label = int(prob > 0.5)
        good = label == want_label and abs(prob - want_prob) < 0.03
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
    ap.add_argument("--batch-size", type=int, default=4)
    ap.add_argument("--batch-tokens", type=int, default=8192,
                    help="max padded tokens per batch (batch_size x max seq len)")
    ap.add_argument("--device", default="auto")
    ap.add_argument("--dtype", default="float16", choices=["float16", "bfloat16", "float32"])
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    device = pick_device(args.device)
    dtype = getattr(torch, args.dtype)
    print(f"device={device.type} dtype={args.dtype} model={MODEL_ID}")
    scorer = Scorer(device, dtype)

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

    # Length-sorted, token-budget batching: batch cost is batch_size x padded
    # length, so mixing a 6k-token doc into a batch of short ones multiplies
    # memory for nothing. Long docs run in progressively smaller batches
    # (worst case alone). Row order does not affect per-subset BAcc.
    for it in todo:
        it["_len"] = len(scorer.tokenizer(it["doc"], add_special_tokens=False)["input_ids"])
    todo.sort(key=lambda it: it["_len"])
    batches, cur = [], []
    for it in todo:
        if cur and (len(cur) + 1) * (it["_len"] + 300) > args.batch_tokens:
            batches.append(cur)
            cur = []
        cur.append(it)
        if len(cur) >= args.batch_size:
            batches.append(cur)
            cur = []
    if cur:
        batches.append(cur)

    rows = []
    t0 = time.time()
    with open(results_path, "a") as out_f:
        for i, batch in enumerate(batches):
            probs = scorer.support_probs([(it["doc"], it["claim"]) for it in batch])
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
            if i % 5 == 0 or done == len(items):
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
        "dtype": args.dtype,
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
