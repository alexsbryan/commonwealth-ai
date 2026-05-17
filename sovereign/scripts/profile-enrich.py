#!/usr/bin/env python3
"""Aggregate `inference_client:` lines from an enrich run into a profile report.

Reads a sovereign enrich build log (from `RUST_LOG=info`) and emits a
markdown breakdown of where wall-clock went: per-phase histograms,
embed-vs-chat split, top-10 longest calls, sum-of-LLM vs wall-clock gap.

Why this exists: the per-call telemetry in
`sovereign/crates/sovereign-cli/src/enrich_cmd/inference_client.rs`
already logs `elapsed_ms`, `tok_per_s`, `phase`, `model` per chat call
(and now `text_len_chars` per embed call). The raw log is human-readable
but the aggregate "where did the time go" answer requires summing the
lines. This script does that summing.

Designed to be lazy about the log format — `tracing` field syntax is
matched with regex rather than full structured parsing, so the script
tolerates ANSI escape codes from `tracing-subscriber` and the occasional
field re-order.

Usage:
    profile-enrich.py /tmp/profile-sep-compatibilism-*.log
    profile-enrich.py log.txt --wall-clock-start "2026-05-17T01:30:00Z" \\
                              --wall-clock-end   "2026-05-17T01:48:30Z"
"""

import argparse
import re
import statistics
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path

# Match `tracing-subscriber` event lines that carry the `inference_client:`
# message. We extract fields lazily via a per-key regex rather than try to
# parse the whole tracing pretty-printer output.
CHAT_OK_RE = re.compile(r"inference_client: /v1/chat/completions ok")
EMBED_OK_RE = re.compile(r"inference_client: /v1/embeddings ok")
CHAT_FAIL_RE = re.compile(r"inference_client: /v1/chat/completions failed")

# Field extractors. `tracing` writes `key=value` for primitive types and
# `key="value"` for quoted strings. The patterns cover both.
def field(name: str, line: str, *, kind: str = "int"):
    """Pull a single `key=value` field from a tracing log line. Returns
    None when missing — the caller decides whether that's load-bearing."""
    if kind == "str":
        # Bare value up to next whitespace, or quoted string.
        m = re.search(rf'\b{re.escape(name)}=([^\s"]+)', line)
        if not m:
            m = re.search(rf'\b{re.escape(name)}="([^"]*)"', line)
        return m.group(1) if m else None
    if kind == "float":
        m = re.search(rf'\b{re.escape(name)}="?(-?\d+(?:\.\d+)?)', line)
        return float(m.group(1)) if m else None
    # int
    m = re.search(rf'\b{re.escape(name)}=(\d+)', line)
    return int(m.group(1)) if m else None


# Also extract the leading ISO-8601 timestamp tracing-subscriber writes so we
# can recover wall clock from the log itself when --wall-clock-* flags aren't
# passed. Format: `2026-05-17T01:30:00.123456Z`. Strip ANSI first.
ANSI = re.compile(r"\x1b\[[0-9;]*[mGKHF]")
TS_RE = re.compile(r"\b(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z")


def parse_log(path: Path):
    """Yield (kind, fields) per matching line. kind in {chat_ok, chat_fail, embed_ok}."""
    with path.open("r", errors="replace") as f:
        for raw in f:
            line = ANSI.sub("", raw)
            ts_m = TS_RE.search(line)
            ts = (
                datetime.fromisoformat(ts_m.group(1)).timestamp()
                if ts_m
                else None
            )
            if CHAT_OK_RE.search(line):
                yield (
                    "chat_ok",
                    {
                        "ts": ts,
                        "phase": field("phase", line, kind="str"),
                        "model": field("model", line, kind="str"),
                        "elapsed_ms": field("elapsed_ms", line) or 0,
                        "completion_tokens": field("completion_tokens", line) or 0,
                        "total_tokens": field("total_tokens", line) or 0,
                        "tok_per_s": field("tok_per_s", line, kind="float") or 0.0,
                        "finish_reason": field("finish_reason", line, kind="str") or "?",
                    },
                )
            elif CHAT_FAIL_RE.search(line):
                yield (
                    "chat_fail",
                    {
                        "ts": ts,
                        "phase": field("phase", line, kind="str"),
                        "model": field("model", line, kind="str"),
                        "elapsed_ms": field("elapsed_ms", line) or 0,
                    },
                )
            elif EMBED_OK_RE.search(line):
                yield (
                    "embed_ok",
                    {
                        "ts": ts,
                        "model": field("model", line, kind="str"),
                        "elapsed_ms": field("elapsed_ms", line) or 0,
                        "text_len_chars": field("text_len_chars", line) or 0,
                        "embed_dim": field("embed_dim", line) or 0,
                    },
                )


def percentile(values, p):
    if not values:
        return 0
    s = sorted(values)
    k = max(0, min(len(s) - 1, int(round((p / 100) * (len(s) - 1)))))
    return s[k]


def hist_row(label, latencies, *, ms_to_s=True):
    if not latencies:
        return f"| {label} | 0 | — | — | — | — | — |"
    total = sum(latencies)
    fmt = lambda v: f"{v/1000:.1f}s" if ms_to_s else f"{v:.0f}ms"
    return (
        f"| {label} | {len(latencies)} | {fmt(total)} | "
        f"{fmt(percentile(latencies, 50))} | "
        f"{fmt(percentile(latencies, 90))} | "
        f"{fmt(percentile(latencies, 99))} | "
        f"{fmt(max(latencies))} |"
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("log_path", type=Path)
    ap.add_argument("--wall-clock-start", help="ISO-8601 timestamp (else inferred from first log line)")
    ap.add_argument("--wall-clock-end", help="ISO-8601 timestamp (else inferred from last log line)")
    args = ap.parse_args()
    if not args.log_path.exists():
        print(f"error: {args.log_path} does not exist", file=sys.stderr)
        sys.exit(2)

    events = list(parse_log(args.log_path))
    if not events:
        print(
            "error: no inference_client lines found — was RUST_LOG=info? "
            "are you on a build that has the embed/chat ok logs?",
            file=sys.stderr,
        )
        sys.exit(1)

    # Wall clock: prefer CLI override, else infer from log timestamps.
    log_ts = [e[1]["ts"] for e in events if e[1].get("ts")]
    wall_start = (
        datetime.fromisoformat(args.wall_clock_start.rstrip("Z")).timestamp()
        if args.wall_clock_start
        else (min(log_ts) if log_ts else None)
    )
    wall_end = (
        datetime.fromisoformat(args.wall_clock_end.rstrip("Z")).timestamp()
        if args.wall_clock_end
        else (max(log_ts) if log_ts else None)
    )
    wall_s = (wall_end - wall_start) if (wall_start and wall_end) else None

    # Aggregate by (kind, phase).
    by_phase = defaultdict(list)  # phase -> [elapsed_ms]
    by_phase_completion = defaultdict(list)
    by_phase_model = defaultdict(set)
    by_phase_finish = defaultdict(lambda: defaultdict(int))  # phase -> {reason: n}
    embed_lat = []
    embed_text_len = []
    failed = []
    for kind, e in events:
        if kind == "chat_ok":
            phase = e["phase"] or "<no phase>"
            by_phase[phase].append(e["elapsed_ms"])
            by_phase_completion[phase].append(e["completion_tokens"])
            by_phase_model[phase].add(e["model"])
            by_phase_finish[phase][e.get("finish_reason", "?")] += 1
        elif kind == "embed_ok":
            embed_lat.append(e["elapsed_ms"])
            embed_text_len.append(e["text_len_chars"])
        elif kind == "chat_fail":
            failed.append(e)

    print(f"# Profile: {args.log_path.name}")
    print()
    chat_total_ms = sum(sum(v) for v in by_phase.values())
    embed_total_ms = sum(embed_lat)
    llm_total_ms = chat_total_ms + embed_total_ms
    print(f"- wall clock: {wall_s:.1f}s ({wall_s/60:.1f}min)" if wall_s else "- wall clock: <unknown>")
    print(f"- chat calls: {sum(len(v) for v in by_phase.values())}  (total {chat_total_ms/1000:.1f}s)")
    print(f"- embed calls: {len(embed_lat)}  (total {embed_total_ms/1000:.1f}s)")
    if wall_s:
        gap = wall_s - llm_total_ms / 1000
        gap_pct = (gap / wall_s) * 100 if wall_s else 0
        print(f"- non-LLM time (orchestration + I/O + clustering math): {gap:.1f}s ({gap_pct:.1f}% of wall)")
        print(f"- LLM utilization (chat+embed wall vs total wall): {(1-gap_pct/100)*100:.1f}%")
    if failed:
        print(f"- **failed calls**: {len(failed)}")
    print()

    print("## Per-phase chat latency")
    print()
    print("| phase | n | total | p50 | p90 | p99 | max |")
    print("|---|---:|---:|---:|---:|---:|---:|")
    for phase in sorted(by_phase.keys(), key=lambda p: -sum(by_phase[p])):
        print(hist_row(phase, by_phase[phase]))
    print()

    print("## Per-phase chat token throughput")
    print()
    print("| phase | n | total_completion_tokens | mean tok/call | mean tok/s |")
    print("|---|---:|---:|---:|---:|")
    for phase in sorted(by_phase.keys(), key=lambda p: -sum(by_phase_completion[p])):
        tokens = by_phase_completion[phase]
        lats = by_phase[phase]
        total_tok = sum(tokens)
        mean_tok = statistics.mean(tokens) if tokens else 0
        # tok/s averaged per-call, not weighted by call duration
        per_call_tps = [
            (t / (l / 1000)) for t, l in zip(tokens, lats) if l > 0
        ]
        mean_tps = statistics.mean(per_call_tps) if per_call_tps else 0
        models = ",".join(sorted(by_phase_model[phase])) or "?"
        print(
            f"| {phase} | {len(tokens)} | {total_tok} | {mean_tok:.0f} | "
            f"{mean_tps:.1f} | (model: {models}) |"
        )
    print()

    print("## Per-phase finish_reason split")
    print()
    print("| phase | n | stop (EOS) | length (max_tokens) | tool_calls | ? |")
    print("|---|---:|---:|---:|---:|---:|")
    for phase in sorted(by_phase.keys(), key=lambda p: -sum(by_phase[p])):
        n = len(by_phase[phase])
        f = by_phase_finish[phase]
        print(
            f"| {phase} | {n} | {f.get('stop', 0)} | {f.get('length', 0)} | "
            f"{f.get('tool_calls', 0)} | {f.get('?', 0)} |"
        )
    print()

    print("## Embed call distribution")
    print()
    print("| n | total | p50 | p90 | p99 | max | mean text_chars |")
    print("|---:|---:|---:|---:|---:|---:|---:|")
    mean_text = statistics.mean(embed_text_len) if embed_text_len else 0
    if embed_lat:
        print(
            f"| {len(embed_lat)} | {sum(embed_lat)/1000:.1f}s | "
            f"{percentile(embed_lat, 50)}ms | {percentile(embed_lat, 90)}ms | "
            f"{percentile(embed_lat, 99)}ms | {max(embed_lat)}ms | "
            f"{mean_text:.0f} |"
        )
    else:
        print("| 0 | — | — | — | — | — | — |")
    print()

    # Top 10 longest individual chat calls — the tail risk
    longest = sorted(
        [(e["elapsed_ms"], e["phase"], e.get("completion_tokens") or 0, e.get("model") or "?")
         for k, e in events if k == "chat_ok"],
        reverse=True,
    )[:10]
    if longest:
        print("## Top 10 longest single chat calls")
        print()
        print("| elapsed | phase | completion_tokens | model |")
        print("|---:|---|---:|---|")
        for ms, ph, tok, mdl in longest:
            print(f"| {ms/1000:.1f}s | {ph} | {tok} | {mdl} |")
        print()

    if failed:
        print("## Failed calls")
        print()
        for e in failed:
            print(f"- phase={e['phase']} model={e['model']} elapsed_ms={e['elapsed_ms']}")
        print()


if __name__ == "__main__":
    main()
