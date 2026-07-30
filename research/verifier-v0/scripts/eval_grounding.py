#!/usr/bin/env python3
"""Grounding-verifier eval harness (M0): LLM-AggreFact / RAGTruth via an
OpenAI-compatible chat endpoint (llama-server Metal, mlx_lm.server, or the
sovereign daemon -- one code path, any backend).

Scores binary support (label 1 = claim supported by doc) as balanced accuracy
per subset, the metric the LLM-AggreFact leaderboard reports. The prompt is
the HalluGuard training interface (JSON instructions/document/claim ->
XML answer), mapped GROUNDED->1, HALLUCINATED_*->0. Parse failures are
recorded and scored as incorrect.

Resumable: per-item results append to results.jsonl in the run dir; reruns
skip completed item ids. Summary lands in summary.json.

Usage:
  eval_grounding.py --run-dir runs/eval-halluguard-gguf \
      --parquet data/llm-aggrefact/test.parquet \
      --base-url http://127.0.0.1:8089/v1 \
      --per-subset 200 --concurrency 4
"""

import argparse
import collections
import json
import os
import random
import re
import sys
import threading
import time
import urllib.request

CLASSES = ("GROUNDED", "HALLUCINATED_INTRINSIC", "HALLUCINATED_EXTRINSIC")
ANSWER_RE = re.compile(
    r"<answer>\s*<classification>\s*(\w+)\s*</classification>\s*"
    r"<justification>(.*?)</justification>\s*</answer>",
    re.DOTALL,
)

# The exact instruction block from HalluGuard-Preferences-76k (its training
# distribution). Kept verbatim -- do not "improve" it.
INSTRUCTIONS = [
    "You will be given a document and a claim.",
    "Decide whether the claim is 'GROUNDED', 'HALLUCINATED_INTRINSIC', or 'HALLUCINATED_EXTRINSIC' based ONLY on the document.",
    "Definitions:",
    "  - GROUNDED: The claim is fully supported by the document. All relevant parts are directly verifiable from the document.",
    "  - HALLUCINATED_INTRINSIC: The claim contradicts what the document states or clearly implies.",
    "  - HALLUCINATED_EXTRINSIC: The claim includes information that is not stated or implied in the document and cannot be verified using only the document (it requires external knowledge).",
    "Justification requirements:",
    "  - Your justification MUST be evidence-grounded.",
    "  - Explicitly refer to the relevant parts of the document (by quoting or paraphrasing them).",
    "  - Explain how these parts SUPPORT, CONTRADICT, or FAIL TO SUPPORT the claim.",
    "  - Do NOT use any external knowledge; rely only on the provided document.",
    "Answer format (VERY IMPORTANT):",
    "  - You MUST respond using EXACTLY the following XML structure:",
    "    <answer>",
    "      <classification>CATEGORY</classification>",
    "      <justification>Your reasoning here</justification>",
    "    </answer>",
    "  - CATEGORY must be ONE of: GROUNDED, HALLUCINATED_INTRINSIC, HALLUCINATED_EXTRINSIC.",
    "  - The <justification> must briefly explain your reasoning and cite evidence from the document.",
    "  - Do NOT add any other text before or after the <answer>...</answer> block.",
    "  - Do NOT add any extra tags or attributes.",
]


def build_prompt(doc: str, claim: str) -> str:
    return json.dumps(
        {"instructions": INSTRUCTIONS, "document": doc, "claim": claim},
        ensure_ascii=False,
    )


def parse_verdict(text: str):
    """Return 1 (grounded), 0 (hallucinated), or None. Reads only after the
    last </think> because models quote the format template while thinking."""
    tail = text.rsplit("</think>", 1)[-1]
    cls = None
    for m in ANSWER_RE.finditer(tail):
        if m.group(1) in CLASSES:
            cls = m.group(1)
    if cls is None:
        return None, None
    return (1 if cls == "GROUNDED" else 0), cls


def chat(base_url, model, prompt, max_tokens, timeout):
    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0.0,
            "max_tokens": max_tokens,
        }
    ).encode()
    req = urllib.request.Request(
        f"{base_url}/chat/completions",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.load(r)
    msg = out["choices"][0]["message"]
    usage = out.get("usage", {})
    # llama-server (thinking-aware templates) splits reasoning into its own
    # field; other backends leave <think> inline. Concatenate so the parser
    # sees one stream either way.
    reasoning = msg.get("reasoning_content") or ""
    content = msg.get("content") or ""
    text = f"<think>{reasoning}</think>{content}" if reasoning else content
    return text, usage


def load_items(source, per_subset, subsets, seed):
    by_subset = collections.defaultdict(list)
    if source.endswith(".jsonl"):
        with open(source) as f:
            for i, line in enumerate(f):
                r = json.loads(line)
                ds = r["dataset"]
                if subsets and ds not in subsets:
                    continue
                by_subset[ds].append(
                    {
                        "id": r.get("id", f"{ds}:{i}"),
                        "subset": ds,
                        "doc": r["doc"],
                        "claim": r["claim"],
                        "label": int(r["label"]),
                    }
                )
    else:
        import pyarrow.parquet as pq

        t = pq.read_table(source)
        cols = {c: t.column(c).to_pylist() for c in ("dataset", "doc", "claim", "label")}
        for i in range(t.num_rows):
            ds = cols["dataset"][i]
            if subsets and ds not in subsets:
                continue
            by_subset[ds].append(
                {
                    "id": f"{ds}:{i}",
                    "subset": ds,
                    "doc": cols["doc"][i],
                    "claim": cols["claim"][i],
                    "label": int(cols["label"][i]),
                }
            )
    rng = random.Random(seed)
    picked = []
    for ds in sorted(by_subset):
        pool = by_subset[ds]
        if per_subset and len(pool) > per_subset:
            # stratified by label so BAcc stays estimable on both classes
            pos = [x for x in pool if x["label"] == 1]
            neg = [x for x in pool if x["label"] == 0]
            rng.shuffle(pos)
            rng.shuffle(neg)
            half = per_subset // 2
            take_pos = pos[:half]
            take_neg = neg[:half]
            # top up from the larger class if the smaller ran short
            short = per_subset - len(take_pos) - len(take_neg)
            if short > 0:
                extra = pos[half:] if len(pos) > len(neg) else neg[half:]
                take_pos.extend(extra[:short] if len(pos) > len(neg) else [])
                take_neg.extend(extra[:short] if len(neg) >= len(pos) else [])
            picked.extend(take_pos + take_neg)
        else:
            picked.extend(pool)
    rng.shuffle(picked)
    return picked


def bacc(rows):
    tp = sum(1 for r in rows if r["label"] == 1 and r["pred"] == 1)
    fn = sum(1 for r in rows if r["label"] == 1 and r["pred"] != 1)
    tn = sum(1 for r in rows if r["label"] == 0 and r["pred"] == 0)
    fp = sum(1 for r in rows if r["label"] == 0 and r["pred"] != 0)
    tpr = tp / (tp + fn) if tp + fn else 0.0
    tnr = tn / (tn + fp) if tn + fp else 0.0
    return {
        "bacc": round(100 * (tpr + tnr) / 2, 2),
        "tpr_supported": round(100 * tpr, 2),
        "tnr_hallucinated": round(100 * tnr, 2),
        "n": len(rows),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-dir", required=True)
    ap.add_argument("--source", "--parquet", dest="source", default="data/llm-aggrefact/test.parquet", help="benchmark rows: parquet or jsonl with dataset/doc/claim/label")
    ap.add_argument("--base-url", default="http://127.0.0.1:8089/v1")
    ap.add_argument("--model", default="verifier")
    ap.add_argument("--per-subset", type=int, default=200)
    ap.add_argument("--subsets", nargs="*", default=None)
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--max-tokens", type=int, default=2560)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--seed", type=int, default=17)
    args = ap.parse_args()

    os.makedirs(args.run_dir, exist_ok=True)
    results_path = os.path.join(args.run_dir, "results.jsonl")
    done = set()
    if os.path.exists(results_path):
        with open(results_path) as f:
            for line in f:
                try:
                    done.add(json.loads(line)["id"])
                except json.JSONDecodeError:
                    pass

    items = [x for x in load_items(args.source, args.per_subset, args.subsets, args.seed) if x["id"] not in done]
    print(f"items to run: {len(items)} (skipping {len(done)} already done)")

    lock = threading.Lock()
    out_f = open(results_path, "a")
    stats = {"done": 0, "parse_fail": 0, "errors": 0, "t0": time.time(), "completion_tokens": 0}

    def work(item):
        prompt = build_prompt(item["doc"], item["claim"])
        try:
            text, usage = chat(args.base_url, args.model, prompt, args.max_tokens, args.timeout)
        except Exception as e:  # network/backend error: record, don't crash the run
            with lock:
                stats["errors"] += 1
                out_f.write(json.dumps({**{k: item[k] for k in ("id", "subset", "label")}, "pred": None, "error": str(e)[:200]}) + "\n")
                out_f.flush()
            return
        pred, cls = parse_verdict(text)
        with lock:
            stats["done"] += 1
            stats["completion_tokens"] += usage.get("completion_tokens", 0)
            if pred is None:
                stats["parse_fail"] += 1
            out_f.write(
                json.dumps(
                    {
                        "id": item["id"],
                        "subset": item["subset"],
                        "label": item["label"],
                        "pred": pred,
                        "cls": cls,
                        "completion_tokens": usage.get("completion_tokens"),
                    }
                )
                + "\n"
            )
            out_f.flush()
            if stats["done"] % 25 == 0:
                dt = time.time() - stats["t0"]
                print(
                    f"{stats['done']}/{len(items)} | {stats['done']/dt*60:.1f} items/min | "
                    f"parse_fail {stats['parse_fail']} | errors {stats['errors']}",
                    flush=True,
                )

    import concurrent.futures as cf

    with cf.ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        list(ex.map(work, items))
    out_f.close()

    # summarize everything on disk (including prior resumed rows)
    rows = []
    with open(results_path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "error" not in r:
                # parse failure scores as incorrect: pred forced to wrong label
                if r["pred"] is None:
                    r = {**r, "pred": 1 - r["label"]}
                rows.append(r)
    by_subset = collections.defaultdict(list)
    for r in rows:
        by_subset[r["subset"]].append(r)
    summary = {
        "base_url": args.base_url,
        "source": args.source,
        "per_subset": args.per_subset,
        "seed": args.seed,
        "subsets": {ds: bacc(v) for ds, v in sorted(by_subset.items())},
    }
    summary["macro_avg_bacc"] = round(
        sum(v["bacc"] for v in summary["subsets"].values()) / max(len(summary["subsets"]), 1), 2
    )
    summary["parse_failures"] = stats["parse_fail"]
    summary["errors"] = stats["errors"]
    with open(os.path.join(args.run_dir, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
