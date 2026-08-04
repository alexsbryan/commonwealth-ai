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

# Tolerant fallbacks, tried in order after ANSWER_RE misses. Rationale: the
# measurement is "did the model reach a verdict", not "did it close every tag".
# Measured 2026-08-02 on the 0.8B probe: it emitted
#   <classification>GROUNDED</classification> ... </justivation>   (typo, no </answer>)
# and with thinking disabled
#   <answer>{"classification":"GROUNDED", ...}                     (JSON body)
# Both are correct verdicts that ANSWER_RE discards wholesale. On the 4B
# baseline the same all-or-nothing rule cost 8.6% of rows and ~6 BAcc points
# (BASELINES.md: strict 70.77 vs excl-pf 76.76).
#
# Deliberately NOT tolerant of a bare class token in prose -- a model that
# writes "CATEGORY must be ONE of: GROUNDED, ..." has not answered. A tag or a
# JSON key is required, and (like ANSWER_RE) the LAST match wins.
CLASSIFICATION_TAG_RE = re.compile(r"<classification>\s*(\w+)\s*</?classification>", re.DOTALL)
CLASSIFICATION_JSON_RE = re.compile(r"[\"']classification[\"']\s*:\s*[\"'](\w+)[\"']", re.DOTALL)

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


# Decoder-enforced answer schema. ARCH_PRINCIPLES §7.6: never ask a model to
# guarantee what code can enforce.
#
# WHY THIS EXISTS. Arm A @118 steps emits `<answer>VERDICT</classification>` --
# it opens <answer>, skips <classification> entirely, then closes
# </classification>. It does this on 2,185 of 2,200 items, i.e. it has CONVERGED
# on a stable malformed template rather than wandered toward a correct one. Its
# verdicts and justifications are sound; only the nesting is wrong. Under the
# tolerant parser (which requires an OPENING <classification>) that is 0.05 macro
# BAcc against the base model's 53.19 -- a 3-point-worse-than-chance score for a
# model that is actually 14 points BETTER on the underlying judgment.
#
# The tolerant parser was fitted to the BASE model's failure modes (see the
# comment block above it). Widening it to admit arm A's new one would be fitting
# the ruler to the arm -- and would have to be re-fitted for every future
# checkpoint's novel malformation. Constraining the decoder is the fix that does
# not need redoing: any checkpoint, trained or not, emits parseable output.
ANSWER_GBNF = r"""
root ::= "<answer>\n  <classification>" cls "</classification>\n  <justification>" just "</justification>\n</answer>"
cls  ::= "GROUNDED" | "HALLUCINATED_INTRINSIC" | "HALLUCINATED_EXTRINSIC"
just ::= [^<>]+
"""


def build_prompt(doc: str, claim: str) -> str:
    return json.dumps(
        {"instructions": INSTRUCTIONS, "document": doc, "claim": claim},
        ensure_ascii=False,
    )


def parse_verdict(text: str):
    """Return 1 (grounded), 0 (hallucinated), or None. Reads only after the
    last </think> because models quote the format template while thinking.

    STRICT: requires a fully well-formed <answer> block. This is the historical
    parser and the one BASELINES.md's headline column was measured with -- do
    not loosen it. Use parse_verdict_tolerant for the format-forgiving read.
    """
    tail = text.rsplit("</think>", 1)[-1]
    cls = None
    for m in ANSWER_RE.finditer(tail):
        if m.group(1) in CLASSES:
            cls = m.group(1)
    if cls is None:
        return None, None
    return (1 if cls == "GROUNDED" else 0), cls


def parse_verdict_tolerant(text: str):
    """Return (pred, cls, how) where how is 'strict' | 'tag' | 'json' | None.

    Same reading window as parse_verdict -- only after the last </think> -- so
    a model reasoning about the template still cannot leak a verdict. The
    difference is only in how much malformed markup around a valid
    classification is forgiven. See the regex block above for why.
    """
    pred, cls = parse_verdict(text)
    if cls is not None:
        return pred, cls, "strict"
    tail = text.rsplit("</think>", 1)[-1]
    for how, rx in (("tag", CLASSIFICATION_TAG_RE), ("json", CLASSIFICATION_JSON_RE)):
        cls = None
        for m in rx.finditer(tail):
            if m.group(1) in CLASSES:
                cls = m.group(1)
        if cls is not None:
            return (1 if cls == "GROUNDED" else 0), cls, how
    return None, None, None


def chat(base_url, model, prompt, max_tokens, timeout, extra=None):
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.0,
        "max_tokens": max_tokens,
    }
    # Sampling/template controls the caller wants recorded as part of the
    # protocol (repeat_penalty, enable_thinking, ...). Empty by default so the
    # historical baseline protocol is unchanged unless a flag asks for it.
    payload.update(extra or {})
    body = json.dumps(payload).encode()
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
    ap.add_argument("--repeat-penalty", type=float, default=None,
                    help="llama-server repeat_penalty. Small models degenerate into "
                         "repetition loops under greedy decoding and never emit a verdict.")
    ap.add_argument("--no-think", action="store_true",
                    help="disable the thinking block (chat_template_kwargs.enable_thinking=false). "
                         "Measured 25-30x faster on the 0.8B and immune to the think-loop, but it "
                         "is a DIFFERENT protocol from the committed baselines -- label runs accordingly.")
    ap.add_argument("--no-save-responses", action="store_true",
                    help="skip responses.jsonl. Default is to save: without the raw text a run "
                         "cannot be re-scored offline, which is what forced a re-run on 2026-08-02.")
    ap.add_argument("--grammar", action="store_true",
                    help="constrain decoding to the <answer> schema (llama-server GBNF). "
                         "A format the parser MUST accept is enforced by the decoder rather "
                         "than hoped for from training -- see ANSWER_GBNF.")
    args = ap.parse_args()

    sampling = {}
    if args.repeat_penalty is not None:
        sampling["repeat_penalty"] = args.repeat_penalty
    if args.no_think:
        sampling["chat_template_kwargs"] = {"enable_thinking": False}
    if args.grammar:
        sampling["grammar"] = ANSWER_GBNF

    os.makedirs(args.run_dir, exist_ok=True)
    results_path = os.path.join(args.run_dir, "results.jsonl")
    responses_path = os.path.join(args.run_dir, "responses.jsonl")
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
    resp_f = None if args.no_save_responses else open(responses_path, "a")
    stats = {"done": 0, "parse_fail": 0, "parse_fail_strict": 0, "rescued": 0,
             "errors": 0, "t0": time.time(), "completion_tokens": 0, "truncated": 0}

    def work(item):
        prompt = build_prompt(item["doc"], item["claim"])
        try:
            text, usage = chat(args.base_url, args.model, prompt, args.max_tokens,
                               args.timeout, sampling)
        except Exception as e:  # network/backend error: record, don't crash the run
            with lock:
                stats["errors"] += 1
                out_f.write(json.dumps({**{k: item[k] for k in ("id", "subset", "label")}, "pred": None, "error": str(e)[:200]}) + "\n")
                out_f.flush()
            return
        # `pred` stays STRICT so this column remains comparable with every
        # committed baseline; the tolerant read is recorded alongside it.
        pred, cls = parse_verdict(text)
        pred_t, cls_t, how = parse_verdict_tolerant(text)
        ctoks = usage.get("completion_tokens") or 0
        with lock:
            stats["done"] += 1
            stats["completion_tokens"] += ctoks
            if pred is None:
                stats["parse_fail_strict"] += 1
            if pred_t is None:
                stats["parse_fail"] += 1
            elif pred is None:
                stats["rescued"] += 1
            # Hitting the cap means the model never finished -- the repetition-loop
            # signature. Counted separately from parse failure so the two causes
            # stay distinguishable in the summary.
            if ctoks >= args.max_tokens:
                stats["truncated"] += 1
            out_f.write(
                json.dumps(
                    {
                        "id": item["id"],
                        "subset": item["subset"],
                        "label": item["label"],
                        "pred": pred,
                        "cls": cls,
                        "pred_tolerant": pred_t,
                        "cls_tolerant": cls_t,
                        "parse_mode": how,
                        "completion_tokens": usage.get("completion_tokens"),
                    }
                )
                + "\n"
            )
            out_f.flush()
            if resp_f is not None:
                resp_f.write(json.dumps({"id": item["id"], "text": text}) + "\n")
                resp_f.flush()
            if stats["done"] % 25 == 0:
                dt = time.time() - stats["t0"]
                print(
                    f"{stats['done']}/{len(items)} | {stats['done']/dt*60:.1f} items/min | "
                    f"parse_fail {stats['parse_fail']} (strict {stats['parse_fail_strict']}, "
                    f"rescued {stats['rescued']}) | truncated {stats['truncated']} | "
                    f"errors {stats['errors']}",
                    flush=True,
                )

    import concurrent.futures as cf

    with cf.ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        list(ex.map(work, items))
    out_f.close()
    if resp_f is not None:
        resp_f.close()

    # summarize everything on disk (including prior resumed rows)
    rows, rows_t = [], []
    for line in open(results_path):
        try:
            r = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "error" in r:
            continue
        # A parse failure scores as incorrect: pred forced to the wrong label.
        # In production an unparseable verdict IS a failed verification, so the
        # strict column stays the floor the fleet actually experiences.
        rows.append({**r, "pred": 1 - r["label"]} if r["pred"] is None else r)
        # Older results.jsonl rows predate the tolerant parser and carry no
        # pred_tolerant key; fall back to strict so resumed runs still summarize.
        pt = r.get("pred_tolerant", r["pred"])
        rows_t.append({**r, "pred": 1 - r["label"] if pt is None else pt})

    def by_sub(rs):
        d = collections.defaultdict(list)
        for r in rs:
            d[r["subset"]].append(r)
        return {ds: bacc(v) for ds, v in sorted(d.items())}

    def macro(sub):
        return round(sum(v["bacc"] for v in sub.values()) / max(len(sub), 1), 2)

    summary = {
        "base_url": args.base_url,
        "source": args.source,
        "per_subset": args.per_subset,
        "seed": args.seed,
        # The protocol is part of the result. A run with --no-think or a
        # repeat_penalty is NOT comparable to one without; record it so a future
        # reader never has to infer it from a shell history.
        "protocol": {
            "model": args.model,
            "max_tokens": args.max_tokens,
            "temperature": 0.0,
            "sampling_overrides": sampling or None,
        },
        "subsets": by_sub(rows),
    }
    summary["macro_avg_bacc"] = macro(summary["subsets"])
    summary["subsets_tolerant"] = by_sub(rows_t)
    summary["macro_avg_bacc_tolerant"] = macro(summary["subsets_tolerant"])
    summary["parse"] = {
        "failures_strict": stats["parse_fail_strict"],
        "failures_tolerant": stats["parse_fail"],
        "rescued_by_tolerant": stats["rescued"],
        "hit_max_tokens": stats["truncated"],
        "scored": len(rows),
    }
    summary["parse_failures"] = stats["parse_fail_strict"]  # back-compat key
    summary["errors"] = stats["errors"]
    with open(os.path.join(args.run_dir, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
