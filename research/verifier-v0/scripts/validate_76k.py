#!/usr/bin/env python3
"""M0 schema validation for HalluGuard-Preferences-76k.

Validates every row of halluguard-main.jsonl against the observed contract:
  {id, prompt, chosen, rejected}
  prompt   = [{role: user, content: JSON{instructions, document, claim}}]
  chosen   = [{role: assistant, content: "<think>...</think>...<answer>
               <classification>C</classification><justification>...</justification></answer>"}]
  rejected = same shape (verdict may be wrong/malformed -- that is why it's rejected)

Emits findings/76k_validation.json and a human summary. Exit 0 iff zero hard
violations (hard = schema breaks on id/prompt/chosen; a malformed *rejected*
answer block is counted but soft, since ORPO only needs it as a worse response).
"""

import argparse
import collections
import glob
import hashlib
import json
import os
import re
import sys

CLASSES = ("GROUNDED", "HALLUCINATED_INTRINSIC", "HALLUCINATED_EXTRINSIC")
ANSWER_RE = re.compile(
    r"<answer>\s*<classification>\s*(\w+)\s*</classification>\s*"
    r"<justification>(.*?)</justification>\s*</answer>",
    re.DOTALL,
)


def default_jsonl() -> str:
    pats = glob.glob(
        os.path.expanduser(
            "~/.cache/huggingface/hub/datasets--lrsbrgrn--HalluGuard-Preferences-76k/"
            "snapshots/*/halluguard-main.jsonl"
        )
    )
    if not pats:
        sys.exit("dataset jsonl not found in HF cache; pass --jsonl")
    return pats[0]


def parse_answer(text: str):
    """Return (classification, justification) or None.

    Responses may quote the answer-format template inside <think>, so parse
    only after the last </think> when present, and take the last valid block.
    """
    tail = text.rsplit("</think>", 1)[-1]
    result = None
    for m in ANSWER_RE.finditer(tail):
        if m.group(1) in CLASSES:
            result = (m.group(1), m.group(2))
    return result


def single_message(field, role):
    """Return content if field is exactly [{role, content:str}], else None."""
    if (
        isinstance(field, list)
        and len(field) == 1
        and isinstance(field[0], dict)
        and field[0].get("role") == role
        and isinstance(field[0].get("content"), str)
    ):
        return field[0]["content"]
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--jsonl", default=None)
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "findings", "76k_validation.json"))
    args = ap.parse_args()
    path = args.jsonl or default_jsonl()

    sha = hashlib.sha256()
    hard, soft = [], []
    ids = set()
    dup_ids = 0
    doc_claim_seen = set()
    dup_doc_claim = 0
    class_counts = collections.Counter()
    agreement = collections.Counter()  # (chosen_cls, rejected_cls_or_None)
    doc_lens, claim_lens, chosen_lens, rejected_lens = [], [], [], []
    think_missing = 0
    n = 0

    with open(path, "rb") as f:
        for lineno, raw in enumerate(f, 1):
            sha.update(raw)
            n += 1
            try:
                d = json.loads(raw)
            except json.JSONDecodeError:
                hard.append((lineno, "line is not JSON"))
                continue

            if set(d.keys()) != {"id", "prompt", "chosen", "rejected"}:
                hard.append((lineno, f"unexpected keys {sorted(d.keys())}"))
                continue
            if d["id"] in ids:
                dup_ids += 1
            ids.add(d["id"])

            p = single_message(d["prompt"], "user")
            c = single_message(d["chosen"], "assistant")
            r = single_message(d["rejected"], "assistant")
            if p is None or c is None or r is None:
                hard.append((lineno, "prompt/chosen/rejected not a single-message list"))
                continue

            try:
                pj = json.loads(p)
                assert set(pj.keys()) == {"instructions", "document", "claim"}
            except Exception:
                hard.append((lineno, "prompt content is not {instructions,document,claim} JSON"))
                continue
            doc_lens.append(len(pj["document"]))
            claim_lens.append(len(pj["claim"]))
            key = hashlib.sha256((pj["document"] + "\x00" + pj["claim"]).encode()).hexdigest()
            if key in doc_claim_seen:
                dup_doc_claim += 1
            doc_claim_seen.add(key)

            chosen_ans = parse_answer(c)
            if chosen_ans is None:
                hard.append((lineno, "chosen answer block malformed"))
                continue
            if "</think>" not in c:
                think_missing += 1
            class_counts[chosen_ans[0]] += 1
            chosen_lens.append(len(c))
            rejected_lens.append(len(r))

            rejected_ans = parse_answer(r)
            if rejected_ans is None:
                soft.append((lineno, "rejected answer block malformed"))
            agreement[(chosen_ans[0], rejected_ans[0] if rejected_ans else "MALFORMED")] += 1

    def pct(x):
        return round(100.0 * x / max(n, 1), 2)

    def dist(v):
        v = sorted(v)
        return {
            "min": v[0],
            "p50": v[len(v) // 2],
            "p95": v[int(len(v) * 0.95)],
            "max": v[-1],
        } if v else {}

    grounded = class_counts["GROUNDED"]
    halluc = class_counts["HALLUCINATED_INTRINSIC"] + class_counts["HALLUCINATED_EXTRINSIC"]
    rej_disagrees = sum(v for (cc, rc), v in agreement.items() if rc != cc)

    report = {
        "source": path,
        "sha256": sha.hexdigest(),
        "rows": n,
        "hard_violations": len(hard),
        "soft_violations": len(soft),
        "hard_examples": hard[:10],
        "soft_examples": soft[:10],
        "duplicate_ids": dup_ids,
        "duplicate_document_claim_pairs": dup_doc_claim,
        "chosen_missing_think": think_missing,
        "class_counts": dict(class_counts),
        "binary_balance": {
            "grounded_pct": pct(grounded),
            "hallucinated_pct": pct(halluc),
        },
        "rejected_verdict_disagrees_with_chosen": {
            "count": rej_disagrees,
            "pct": pct(rej_disagrees),
        },
        "agreement_matrix": {f"{cc}->{rc}": v for (cc, rc), v in sorted(agreement.items())},
        "char_lengths": {
            "document": dist(doc_lens),
            "claim": dist(claim_lens),
            "chosen": dist(chosen_lens),
            "rejected": dist(rejected_lens),
        },
    }

    out = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(report, f, indent=2)

    print(f"rows={n} hard={len(hard)} soft={len(soft)} dup_ids={dup_ids} dup_doc_claim={dup_doc_claim}")
    print(f"classes: {dict(class_counts)}")
    print(f"binary balance: grounded {pct(grounded)}% / hallucinated {pct(halluc)}%")
    print(f"rejected disagrees with chosen: {rej_disagrees} ({pct(rej_disagrees)}%)")
    print(f"report -> {out}")
    return 0 if not hard else 1


if __name__ == "__main__":
    sys.exit(main())
