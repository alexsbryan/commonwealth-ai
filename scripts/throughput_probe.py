#!/usr/bin/env python3
"""Throughput probe — streaming latency/throughput for an OpenAI-compatible endpoint.

The instrument for the heterogeneous-distribution experiment
(docs/QWEN122B_HETEROGENEOUS_EXPERIMENT.md) and the SHARED_MODEL tok/s gate. Unlike
mtp-probe.sh (wall-time only) or rpc-distributed-e2e.sh (proves the chain, measures no
t/s), this streams a fixed completion and times it token by token:

  * TTFT            — request sent -> first content token (ms)
  * decode t/s      — steady-state generation, EXCLUDING TTFT (tokens after the first
                      over the gap between first and last token)
  * inter-token p50/p95 (ms) — where network jitter shows up on the distributed arms
  * prefill t/s     — prompt_tokens / TTFT (proxy; use a long prompt to make it real)

Endpoint-agnostic: point --url at the Sovereign daemon (A0/B1/B2) or a raw
llama-server (B3). Greedy (temperature 0) by default so runs are comparable and the
cross-backend fidelity check is meaningful.

stdlib only — no pip deps. Human summary to stderr; with --json, one aggregate JSON
line to stdout (so `probe ... --json >> results.jsonl` just works).

Usage:
  scripts/throughput_probe.py --model primary --max-tokens 256 --trials 5
  scripts/throughput_probe.py --url http://localhost:8080 --model qwen --json --label B3
"""
import argparse
import json
import statistics
import sys
import time
import urllib.request

DEFAULT_PROMPT = (
    "Explain, in about 200 words, how a mixture-of-experts transformer routes tokens "
    "to experts and why only a fraction of parameters activate per token. Then give one "
    "concrete tradeoff versus a dense model of the same total parameter count."
)


def log(*a):
    print(*a, file=sys.stderr, flush=True)


def one_trial(url, path, model, prompt, max_tokens, temperature, timeout):
    """Run a single streaming completion; return per-trial metrics dict."""
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": True,
        "stream_options": {"include_usage": True},
    }).encode()
    req = urllib.request.Request(
        url.rstrip("/") + path,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )

    token_times = []          # perf_counter at each content token
    text = []                 # assembled completion (fidelity check)
    usage = None
    streamed = False
    t0 = time.perf_counter()
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        ctype = resp.headers.get("Content-Type", "")
        if "text/event-stream" not in ctype:
            # Server didn't stream — fall back to whole-body wall-time (no TTFT/ITL).
            payload = json.loads(resp.read().decode())
            wall = time.perf_counter() - t0
            msg = payload.get("choices", [{}])[0].get("message", {}).get("content", "")
            usage = payload.get("usage")
            n = (usage or {}).get("completion_tokens")
            return {
                "streamed": False, "wall_s": wall, "ttft_ms": None,
                "decode_tps": (n / wall if n and wall else None),
                "n_tokens": n, "itl_ms": [], "usage": usage, "text": msg,
            }
        streamed = True
        for raw in resp:                      # yields lines as they arrive
            line = raw.decode("utf-8", "replace").strip()
            if not line.startswith("data:"):
                continue
            data = line[5:].strip()
            if data == "[DONE]":
                break
            try:
                obj = json.loads(data)
            except json.JSONDecodeError:
                continue
            if obj.get("usage"):
                usage = obj["usage"]
            choices = obj.get("choices") or []
            if not choices:
                continue
            piece = (choices[0].get("delta") or {}).get("content")
            if piece:
                token_times.append(time.perf_counter())
                text.append(piece)

    if not token_times:
        return {"streamed": streamed, "wall_s": time.perf_counter() - t0,
                "ttft_ms": None, "decode_tps": None, "n_tokens": 0,
                "itl_ms": [], "usage": usage, "text": ""}

    ttft_ms = (token_times[0] - t0) * 1000.0
    n = len(token_times)
    gen_window = token_times[-1] - token_times[0]
    decode_tps = ((n - 1) / gen_window) if (n >= 2 and gen_window > 0) else None
    itl_ms = [(token_times[i] - token_times[i - 1]) * 1000.0 for i in range(1, n)]
    return {
        "streamed": True, "wall_s": time.perf_counter() - t0, "ttft_ms": ttft_ms,
        "decode_tps": decode_tps, "n_tokens": n, "itl_ms": itl_ms,
        "usage": usage, "text": "".join(text),
    }


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    k = (len(xs) - 1) * (p / 100.0)
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def med_min_max(xs):
    xs = [x for x in xs if x is not None]
    if not xs:
        return None
    return (statistics.median(xs), min(xs), max(xs))


def main():
    ap = argparse.ArgumentParser(description="Streaming throughput probe")
    ap.add_argument("--url", default="http://localhost:9741")
    ap.add_argument("--path", default="/v1/chat/completions")
    ap.add_argument("--model", default="primary", help='OpenAI model field; "primary" hits the thoughtful slot')
    ap.add_argument("--prompt", default=DEFAULT_PROMPT)
    ap.add_argument("--prompt-file")
    ap.add_argument("--max-tokens", type=int, default=256)
    ap.add_argument("--temperature", type=float, default=0.0)
    ap.add_argument("--trials", type=int, default=5)
    ap.add_argument("--warmup", type=int, default=1, help="discarded trials before measuring")
    ap.add_argument("--timeout", type=float, default=600.0)
    ap.add_argument("--label", default="")
    ap.add_argument("--json", action="store_true", help="emit one aggregate JSON line to stdout")
    args = ap.parse_args()

    prompt = args.prompt
    if args.prompt_file:
        with open(args.prompt_file) as f:
            prompt = f.read()

    log(f"probe: {args.trials} trials (+{args.warmup} warmup) x max_tokens={args.max_tokens} "
        f"temp={args.temperature} model={args.model!r} -> {args.url}{args.path}")

    warm, first_text = [], None
    for i in range(args.warmup + args.trials):
        tag = "warmup" if i < args.warmup else f"trial {i - args.warmup + 1}/{args.trials}"
        try:
            m = one_trial(args.url, args.path, args.model, prompt,
                          args.max_tokens, args.temperature, args.timeout)
        except Exception as e:  # noqa: BLE001 — surface any transport/HTTP error per trial
            log(f"  {tag}: ERROR {type(e).__name__}: {e}")
            continue
        ttft = f"{m['ttft_ms']:.0f}ms" if m["ttft_ms"] is not None else "n/a"
        dtps = f"{m['decode_tps']:.2f}" if m["decode_tps"] is not None else "n/a"
        log(f"  {tag}: ttft={ttft} decode={dtps} t/s tokens={m['n_tokens']} "
            f"{'(non-streaming fallback)' if not m['streamed'] else ''}")
        if i >= args.warmup:
            warm.append(m)
            if first_text is None:
                first_text = m["text"]

    if not warm:
        log("probe: no successful measured trials")
        sys.exit(1)

    decode = med_min_max([m["decode_tps"] for m in warm])
    ttft = med_min_max([m["ttft_ms"] for m in warm])
    all_itl = [x for m in warm for x in m["itl_ms"]]
    usage = next((m["usage"] for m in warm if m["usage"]), None)
    prompt_tokens = (usage or {}).get("prompt_tokens")
    prefill_tps = None
    if prompt_tokens and ttft:
        prefill_tps = prompt_tokens / (ttft[0] / 1000.0)
    # fidelity: did every measured trial produce identical text (greedy determinism)?
    identical = len({m["text"] for m in warm}) == 1

    log("")
    log(f"=== RESULT{(' [' + args.label + ']') if args.label else ''} "
        f"(n={len(warm)}, median [min,max]) ===")
    if decode:
        log(f"  decode      {decode[0]:.2f} t/s  [{decode[1]:.2f}, {decode[2]:.2f}]")
    if ttft:
        log(f"  TTFT        {ttft[0]:.0f} ms   [{ttft[1]:.0f}, {ttft[2]:.0f}]")
    log(f"  ITL p50/p95 {pct(all_itl, 50):.1f} / {pct(all_itl, 95):.1f} ms" if all_itl else "  ITL         n/a")
    if prefill_tps:
        log(f"  prefill     {prefill_tps:.1f} t/s  ({prompt_tokens} prompt tokens / TTFT)")
    log(f"  greedy fidelity: {'identical across trials' if identical else 'DIVERGED across trials'}")

    if args.json:
        out = {
            "label": args.label,
            "url": args.url,
            "model": args.model,
            "trials": len(warm),
            "max_tokens": args.max_tokens,
            "decode_tps_median": decode[0] if decode else None,
            "decode_tps_min": decode[1] if decode else None,
            "decode_tps_max": decode[2] if decode else None,
            "ttft_ms_median": ttft[0] if ttft else None,
            "itl_p50_ms": pct(all_itl, 50),
            "itl_p95_ms": pct(all_itl, 95),
            "prefill_tps": prefill_tps,
            "prompt_tokens": prompt_tokens,
            "greedy_identical": identical,
        }
        print(json.dumps(out), flush=True)


if __name__ == "__main__":
    main()
