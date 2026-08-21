#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""AA-Omniscience runner -- a (harness, model) pair leaderboard.

Terminal-Bench's shape, applied to knowledge reliability: the 600-question
bank is harness-agnostic, and a HARNESS is a thin adapter that takes an item
and returns an answer string. `naked` is a registered harness, not a
privileged baseline -- it is the row that happens to match AA's published
protocol, and every other row is read against it.

Two phases, separately resumable, because the judge is the instrument we
expect to swap: answers are expensive and stable, grades are cheap and
provisional. `--rejudge` re-grades a completed run without re-asking anything.

Reading a number off this lane (bench/external/README.md conventions):
  * never read a score whose `coverage` is below 1.0
  * an endpoint error rate above 2% exits 4 -- could-not-judge, not a score
  * `oi_official` and `oi_taxed` are always both reported; the tax is ours
"""
import argparse, csv, json, pathlib, sys, time, urllib.error, urllib.request
from collections import Counter, defaultdict

from prompts import OMNISCIENCE_ANSWER_PROMPT, OMNISCIENCE_GRADER_TEMPLATE
from score import DEFAULT_TAX, GRADES, summarize

BANK = pathlib.Path(__file__).parent / "AA-Omniscience_dataset_public.csv"
LETTER = {"A": "CORRECT", "B": "INCORRECT", "C": "PARTIAL_ANSWER", "D": "NOT_ATTEMPTED"}


def chat(base_url, model, messages, max_tokens, temperature, timeout, seed=None):
    body = {"model": model, "messages": messages, "max_tokens": max_tokens,
            "temperature": temperature}
    if seed is not None:
        body["seed"] = seed
    req = urllib.request.Request(
        f"{base_url}/chat/completions", json.dumps(body).encode(),
        {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as fh:
        d = json.load(fh)
    ch = d["choices"][0]
    return (ch["message"].get("content") or "").strip(), {
        "finish_reason": ch.get("finish_reason"),
        "completion_tokens": d.get("usage", {}).get("completion_tokens"),
    }


# ── harnesses ────────────────────────────────────────────────────────────────
# A harness is (item, args) -> (answer_text, meta). Adding A1/A2 is a function
# here plus a registry line -- not a change to anything above.

def harness_naked(item, args):
    """AA's published protocol: official system prompt, no retrieval, no tools,
    no gate. The leaderboard-comparable row, modulo quantisation and judge."""
    sys_prompt = OMNISCIENCE_ANSWER_PROMPT.format(
        domain=item["domain"], topic=item["topic"])
    return chat(args.base_url, args.model,
                [{"role": "system", "content": sys_prompt},
                 {"role": "user", "content": item["question"]}],
                args.max_tokens, args.temperature, args.timeout, args.seed)


HARNESSES = {"naked": harness_naked}


# ── bank ─────────────────────────────────────────────────────────────────────

def load_bank(limit):
    rows = list(csv.DictReader(BANK.open()))
    if not limit or limit >= len(rows):
        return rows
    by_domain = defaultdict(list)
    for r in rows:
        by_domain[r["domain"]].append(r)
    per = max(1, limit // len(by_domain))
    out = []
    for dom in sorted(by_domain):
        items = by_domain[dom]
        # Evenly spaced, not the first k -- topics are contiguous in the CSV,
        # so a head slice would silently sample 2 topics out of 7 per domain.
        stride = max(1, len(items) // per)
        out.extend(items[::stride][:per])
    return sorted(out, key=lambda r: int(r["question_id"]))[:limit]


def judge(item, answer, args):
    prompt = OMNISCIENCE_GRADER_TEMPLATE.format(
        question=item["question"], target=item["answer"], predicted_answer=answer)
    for attempt in range(2):
        txt, _ = chat(args.base_url, args.judge_model,
                      [{"role": "user", "content": prompt}],
                      args.judge_max_tokens, 0.0, args.timeout, args.seed)
        for ch in txt.strip().upper():
            if ch in LETTER:
                return LETTER[ch], txt
    return None, txt  # unparseable after a retry == could-not-judge, never a grade


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--model", required=True, help="model id as /v1/models reports it")
    p.add_argument("--harness", default="naked", choices=sorted(HARNESSES))
    p.add_argument("--judge-model", default=None, help="default: --model (SELF-JUDGING; see PREREG)")
    p.add_argument("--base-url", default="http://localhost:9741/v1")
    p.add_argument("--out", default=None)
    p.add_argument("--limit", type=int, default=0, help="stratified subsample; 0 = all 600")
    p.add_argument("--tax", type=float, default=DEFAULT_TAX)
    p.add_argument("--max-tokens", type=int, default=512)
    p.add_argument("--judge-max-tokens", type=int, default=8)
    p.add_argument("--temperature", type=float, default=0.0)
    p.add_argument("--seed", type=int, default=1729)
    p.add_argument("--timeout", type=float, default=300.0)
    p.add_argument("--resume", action="store_true", help="keep rows already in <out>/rows.jsonl")
    p.add_argument("--rejudge", action="store_true", help="re-grade existing answers, ask nothing")
    args = p.parse_args()
    args.judge_model = args.judge_model or args.model

    slug = f"{args.harness}--{args.model.replace('/', '_')}"
    out_dir = pathlib.Path(args.out or (pathlib.Path(__file__).parent / "runs" / slug))
    out_dir.mkdir(parents=True, exist_ok=True)
    rows_path = out_dir / "rows.jsonl"

    prior = {}
    if (args.resume or args.rejudge) and rows_path.exists():
        for line in rows_path.read_text().splitlines():
            if line.strip():
                r = json.loads(line)
                prior[r["question_id"]] = r

    bank = load_bank(args.limit)
    print(f"bank {len(bank)} · harness {args.harness} · model {args.model} · "
          f"judge {args.judge_model} · tax {args.tax}", flush=True)

    results, t0 = [], time.time()
    for n, item in enumerate(bank, 1):
        qid = int(item["question_id"])
        row = dict(prior.get(qid) or {}, question_id=qid,
                   domain=item["domain"], topic=item["topic"])
        try:
            if not (row.get("answer") and (args.resume or args.rejudge)):
                row["answer"], row["meta"] = harness_fn(args)(item, args)
                row["error"] = None
            if row.get("answer") is not None and (args.rejudge or not row.get("grade")):
                row["grade"], row["judge_raw"] = judge(item, row["answer"], args)
        except (urllib.error.URLError, OSError, KeyError, TimeoutError) as e:
            row["error"] = f"{type(e).__name__}: {e}"
            row["grade"] = None
        results.append(row)
        if n % 10 == 0 or n == len(bank):
            done = sum(1 for r in results if r.get("grade"))
            print(f"  {n}/{len(bank)} graded={done} "
                  f"elapsed={time.time()-t0:.0f}s", flush=True)

    with rows_path.open("w") as fh:
        for r in results:
            fh.write(json.dumps(r) + "\n")

    counts = Counter(r["grade"] for r in results if r.get("grade") in GRADES)
    n_err = sum(1 for r in results if r.get("error"))
    n_unjudged = sum(1 for r in results if not r.get("error") and not r.get("grade"))
    coverage = sum(counts.values()) / len(results) if results else 0.0
    summary = dict(summarize(counts, args.tax),
                   harness=args.harness, model=args.model,
                   judge_model=args.judge_model,
                   judge_is_official=False,          # official is Gemini 2.5 Flash
                   judge_substitution="local daemon judge; see PREREG.md §judge",
                   bank_size=len(results), coverage=round(coverage, 4),
                   error_rate=round(n_err / max(len(results), 1), 4),
                   unparseable_judge_rate=round(n_unjudged / max(len(results), 1), 4),
                   wall_secs=round(time.time() - t0, 1))
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    print(json.dumps(summary, indent=2))
    if summary["error_rate"] > 0.02:
        print(f"\nCOULD-NOT-JUDGE: error rate {summary['error_rate']:.1%} > 2%.", file=sys.stderr)
        return 4
    if coverage < 1.0:
        print(f"\nCOULD-NOT-JUDGE: coverage {coverage:.1%} < 100%; do not read this score.",
              file=sys.stderr)
        return 4
    return 0


def harness_fn(args):
    return HARNESSES[args.harness]


if __name__ == "__main__":
    sys.exit(main())
