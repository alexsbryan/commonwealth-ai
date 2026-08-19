#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["datasets>=3.0", "httpx>=0.27"]
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""RewardBench 2 as a generative-judge lane against an OpenAI-compatible endpoint.

The external ruler for the comaintainer seat. `gym/comaintainer` scores
judgment on 303 episodes mined from this repo; this scores the same
faculty on AI2's public bank, so the two can be rank-correlated and the
gym's external validity established (or not).

Best-of-N: each item carries one or more `chosen` completions and
several `rejected`. The judge sees them shuffled and replies with a
single number. Correct = the picked index is a `chosen`.

Honesty rules inherited from the gym:
  - a reply that does not parse is `malformed`, counted as WRONG and
    reported in its own column. It is never retried into a pass.
  - completion order is shuffled per item from `--seed`, so a model
    with position bias cannot ride it. The permutation is recorded.
  - the `Ties` subset uses a different official metric; it is scored
    here under the same best-of-N rule but reported separately and
    excluded from the headline macro average.

Usage:
    ./run.py --model Qwen3-8B-Q4_K_M --limit 200
    ./run.py --model FINAL-Bench_Darwin-36B-Opus-Q6_K --out runs/darwin
"""

from __future__ import annotations

import argparse
import asyncio
import json
import random
import re
import statistics
import sys
import time
from collections import defaultdict
from pathlib import Path

import httpx
from datasets import load_dataset

DATASET = "allenai/reward-bench-2"
SPLIT = "test"

SYSTEM = "You compare candidate responses and pick the best one."

TEMPLATE = """{prompt}

{candidates}

Which response above is best? End your reply with a line reading exactly:
Answer: <number>"""

# Reasoning models emit a scratchpad before answering. Parsing the FIRST
# integer out of that is how a bench manufactures an answer the model
# never gave — the 2026-08-18 smoke run scored a model that had only
# written "1. **Analyze the Request:**". So: strip think-blocks, take the
# tagged answer, and fall back to the LAST standalone integer only.
# Anything else is malformed, which is a wrong answer with its own column.
_THINK = re.compile(r"<think>.*?</think>|<thinking>.*?</thinking>", re.S | re.I)
_TAGGED = re.compile(r"answer\s*[:=]\s*\[?\s*([0-9]{1,2})", re.I)
_STANDALONE = re.compile(r"(?<![.\w])([0-9]{1,2})(?![.\w])")


def build_item(row: dict, seed: int) -> dict:
    """Shuffle chosen+rejected into a numbered list with a recorded permutation."""
    chosen = list(row["chosen"])
    rejected = list(row["rejected"])
    pool = [(c, True) for c in chosen] + [(r, False) for r in rejected]
    rng = random.Random(f"{seed}:{row['id']}")
    rng.shuffle(pool)
    return {
        "id": str(row["id"]),
        "subset": row["subset"],
        "prompt": row["prompt"],
        "candidates": [text for text, _ in pool],
        "correct_indices": [i + 1 for i, (_, ok) in enumerate(pool) if ok],
    }


def render(item: dict) -> str:
    blocks = [f"[{i + 1}] {text}" for i, text in enumerate(item["candidates"])]
    return TEMPLATE.format(prompt=item["prompt"], candidates="\n\n".join(blocks))


def parse_tagged_only(reply: str, n: int) -> int | None:
    """The tagged answer, or nothing. Used when the reply was truncated."""
    body = _THINK.sub(" ", reply)
    for raw in reversed(_TAGGED.findall(body)):
        v = int(raw)
        if 1 <= v <= n:
            return v
    return None


def parse_pick(reply: str, n: int) -> int | None:
    body = _THINK.sub(" ", reply)
    tagged = _TAGGED.findall(body)
    for raw in reversed(tagged):
        v = int(raw)
        if 1 <= v <= n:
            return v
    # No tag: the last standalone in-range integer is the model's answer
    # if anything is. A list marker ("1.") is excluded by the lookahead.
    for m in reversed(list(_STANDALONE.finditer(body))):
        v = int(m.group(1))
        if 1 <= v <= n:
            return v
    return None


async def judge_one(
    client: httpx.AsyncClient,
    base_url: str,
    model: str,
    item: dict,
    max_tokens: int,
    temperature: float,
) -> dict:
    t0 = time.monotonic()
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": render(item)},
        ],
        "max_tokens": max_tokens,
        "temperature": temperature,
    }
    # Retry TRANSPORT failures only (503 = daemon slots busy, timeouts).
    # A parsed-but-wrong answer is never retried — that would be tuning
    # the instrument until it agrees.
    reply, err, attempts, finish = "", None, 0, ""
    for attempt in range(5):
        attempts = attempt + 1
        try:
            r = await client.post(f"{base_url}/chat/completions", json=body)
            if r.status_code in (429, 500, 502, 503, 504):
                err = f"HTTP {r.status_code}"
                # The daemon sheds with `retry_after_secs` when its slot
                # queue is full. Honour it — blind exponential backoff
                # either hammers it early or idles long after it freed.
                wait = min(2**attempt, 20)
                try:
                    hinted = r.json().get("retry_after_secs")
                    if isinstance(hinted, (int, float)) and hinted > 0:
                        wait = min(float(hinted) + 1.0, 90.0)
                except Exception:  # noqa: BLE001 — body may not be JSON
                    pass
                await asyncio.sleep(wait)
                continue
            r.raise_for_status()
            choice = r.json()["choices"][0]
            reply = choice["message"]["content"] or ""
            finish = choice.get("finish_reason") or ""
            err = None
            break
        except (httpx.TimeoutException, httpx.TransportError) as exc:
            err = f"{type(exc).__name__}: {exc}"
            await asyncio.sleep(min(2**attempt, 20))
        except Exception as exc:  # noqa: BLE001 — an endpoint error is a datum
            err = f"{type(exc).__name__}: {exc}"
            break

    n = len(item["candidates"])
    # A reply cut off at max_tokens has not answered. Falling back to the
    # last integer in its scratchpad is how the first version of this
    # scorer manufactured picks out of prose ("400 meters", "point 2").
    # Truncated + untagged = malformed, which is a wrong answer that says
    # so, not an invented one.
    truncated = finish == "length"
    if err is not None:
        pick = None
    elif truncated:
        pick = parse_tagged_only(reply, n)
    else:
        pick = parse_pick(reply, n)
    return {
        "id": item["id"],
        "subset": item["subset"],
        "n_candidates": n,
        "correct_indices": item["correct_indices"],
        "pick": pick,
        "correct": pick is not None and pick in item["correct_indices"],
        "malformed": err is None and pick is None,
        "error": err,
        "attempts": attempts,
        "finish_reason": finish,
        "truncated": truncated,
        "raw_head": reply[:400],
        "raw_tail": reply[-400:],
        "wall_ms": int((time.monotonic() - t0) * 1000),
    }


async def run(args: argparse.Namespace) -> int:
    ds = load_dataset(DATASET, split=SPLIT)
    if args.subset:
        wanted = {s.strip().lower() for s in args.subset.split(",")}
        ds = ds.filter(lambda r: r["subset"].lower() in wanted)
    rows = list(ds)
    if args.limit and args.limit < len(rows):
        # Stratified by subset so a --limit run stays representative.
        by_subset: dict[str, list] = defaultdict(list)
        for r in rows:
            by_subset[r["subset"]].append(r)
        rng = random.Random(args.seed)
        for v in by_subset.values():
            rng.shuffle(v)
        take, i = [], 0
        while len(take) < args.limit:
            added = False
            for v in by_subset.values():
                if i < len(v) and len(take) < args.limit:
                    take.append(v[i])
                    added = True
            if not added:
                break
            i += 1
        rows = take

    items = [build_item(r, args.seed) for r in rows]

    # Resume: a long run must survive being paused. Rows already on disk
    # for this (model, seed) are kept and their items skipped. Without
    # this, any interruption costs the whole run — which is what made
    # pausing for a competing measurement expensive on 2026-08-18.
    prior: list[dict] = []
    prior_path = Path(args.out) / "rows.jsonl"
    if args.resume and prior_path.exists():
        seen = set()
        for line in prior_path.read_text().splitlines():
            if line.strip():
                d = json.loads(line)
                # Never resume an errored row — re-ask it.
                if d.get("error") is None:
                    prior.append(d)
                    seen.add(d["id"])
        items = [it for it in items if it["id"] not in seen]
        print(
            f"resume: {len(prior)} rows kept, {len(items)} remaining",
            file=sys.stderr,
        )
    print(
        f"rewardbench2: {len(items)} items · model={args.model} · "
        f"concurrency={args.concurrency} · seed={args.seed}",
        file=sys.stderr,
    )

    sem = asyncio.Semaphore(args.concurrency)
    results: list[dict] = []
    t0 = time.monotonic()
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    live = (out_dir / "rows.jsonl").open("a" if args.resume else "w")
    results.extend(prior)

    async with httpx.AsyncClient(timeout=args.timeout) as client:

        async def worker(item: dict) -> None:
            async with sem:
                res = await judge_one(
                    client, args.base_url, args.model, item, args.max_tokens, args.temperature
                )
                results.append(res)
                live.write(json.dumps(res) + "\n")
                live.flush()
                done = len(results)
                if done % 25 == 0 or done == len(items) + len(prior):
                    acc = sum(r["correct"] for r in results) / done
                    print(
                        f"  {done}/{len(items)}  running acc={acc:.1%}",
                        file=sys.stderr,
                    )

        await asyncio.gather(*(worker(it) for it in items))

    wall = time.monotonic() - t0
    live.close()
    with (out_dir / "rows.jsonl").open("w") as fh:
        for r in sorted(results, key=lambda x: x["id"]):
            fh.write(json.dumps(r) + "\n")

    per_subset: dict[str, list[dict]] = defaultdict(list)
    for r in results:
        per_subset[r["subset"]].append(r)

    # ARCH 18.2 — four verdicts, not two. An item the endpoint never
    # answered is could-not-judge; folding it into the wrong column
    # drags every score toward chance and blames the model for the
    # daemon. Accuracy is over ANSWERED items; coverage is reported
    # beside it and gates the run.
    def answered(rs: list[dict]) -> list[dict]:
        return [x for x in rs if x["error"] is None]

    def acc(rs: list[dict]) -> float:
        a = answered(rs)
        return sum(x["correct"] for x in a) / len(a) if a else 0.0

    # Ties uses a different official metric upstream; keep it out of the
    # headline so this number stays comparable to the leaderboard shape.
    scored = {k: v for k, v in per_subset.items() if k.lower() != "ties"}
    macro = statistics.mean(acc(v) for v in scored.values() if answered(v)) if scored else 0.0
    n_answered = len(answered(results))
    coverage = n_answered / len(results) if results else 0.0

    summary = {
        "dataset": DATASET,
        "model": args.model,
        "base_url": args.base_url,
        "n": len(results),
        "n_answered": n_answered,
        "coverage": round(coverage, 4),
        "seed": args.seed,
        "temperature": args.temperature,
        "macro_accuracy_excl_ties": round(macro, 4),
        "micro_accuracy_all": round(acc(results), 4),
        "malformed_rate": round(
            sum(r["malformed"] for r in results) / max(n_answered, 1), 4
        ),
        "truncated_rate": round(
            sum(r.get("truncated") for r in results) / max(n_answered, 1), 4
        ),
        "could_not_judge_rate": round(1.0 - coverage, 4),
        "wall_seconds": round(wall, 1),
        "per_subset": {
            k: {
                "n": len(v),
                "n_answered": len(answered(v)),
                "accuracy": round(acc(v), 4),
            }
            for k, v in sorted(per_subset.items())
        },
    }
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    print(json.dumps(summary, indent=2))
    if coverage < 0.95:
        print(
            f"\nCOULD-NOT-JUDGE: only {coverage:.1%} of items were answered "
            f"({len(results) - n_answered} endpoint failures). The accuracy above "
            "is over answered items only and is NOT a comparable score — "
            "lower --concurrency and re-run.",
            file=sys.stderr,
        )
        return 4
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--model", required=True, help="model id as the daemon reports it in /v1/models")
    p.add_argument("--base-url", default="http://localhost:9741/v1")
    p.add_argument("--out", default=None, help="output dir (default runs/<model-slug>)")
    p.add_argument("--limit", type=int, default=0, help="stratified subsample; 0 = full bank")
    p.add_argument("--subset", default=None, help="comma-separated subsets, e.g. Factuality,Precise IF")
    # The daemon sheds with `local_queue_full` at queue position 1, so
    # concurrency >1 buys nothing but 503s and backoff — measured
    # 2026-08-18: at 3, ~15 min bought 50 items and the excess workers
    # only retried. Raise only after confirming a deeper slot queue.
    p.add_argument("--concurrency", type=int, default=1)
    p.add_argument("--timeout", type=float, default=300.0)
    # Reasoning models need room to REACH the answer line; 16 truncated
    # Qwopus3.5-4B mid-scratchpad and the old parser scored the scratchpad.
    # The 27B reasons for hundreds of tokens before answering, so 512 was
    # still clipping some replies — `truncated` in rows.jsonl reports it.
    p.add_argument("--max-tokens", type=int, default=1536)
    p.add_argument("--temperature", type=float, default=0.0)
    p.add_argument("--seed", type=int, default=1729)
    p.add_argument(
        "--resume",
        action="store_true",
        help="keep answered rows already in <out>/rows.jsonl and ask only the rest",
    )
    args = p.parse_args()
    if args.out is None:
        slug = re.sub(r"[^A-Za-z0-9._-]+", "-", args.model)
        args.out = str(Path(__file__).parent / "runs" / slug)
    return asyncio.run(run(args))


if __name__ == "__main__":
    raise SystemExit(main())
