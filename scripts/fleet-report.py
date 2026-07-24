#!/usr/bin/env python3
"""Weekly fleet report — context-spend + split-protocol adoption, per session and fleet-wide.

Composes existing glassbox surfaces instead of reparsing transcripts:
  - `sovereign cache-audit --json`          per-session spend table (cost, cache share, acquisition)
  - `sovereign cache-audit --ramp`          successor ramp gate (text; --json not supported there)
  - `sovereign cache-audit --counterfactual` fleet lever sizes (text; --json not supported there)
  - ~/.sovereign/sessions/split-events.jsonl  red/yellow crossings from the split-enforce hook
  - ~/.sovereign/sessions/<id>/frame.md       frame provenance + freshness at session end
  - project transcripts                        commit count only (streamed, line-filtered first)

Writes markdown to ~/.sovereign/reports/fleet-<date>.md plus a machine-readable
fleet-<date>.json sidecar; the previous sidecar (if any) drives trend deltas.
Stdlib only, like the other cache-audit consumers.
"""

import argparse
import glob
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

SESSIONS_DIR = os.path.expanduser("~/.sovereign/sessions")
SPLIT_EVENTS = os.path.join(SESSIONS_DIR, "split-events.jsonl")
DEFAULT_OUT = os.path.expanduser("~/.sovereign/reports")

RAMP_GATE_TOKENS = 5000  # split-safety gate: successor ramps <=5k raw, 0 repeats
RED_HONOR_S = 1800       # session end within 30min of first red = split honored


def run_audit(args_list, cwd):
    cmd = ["sovereign", "cache-audit"] + args_list
    env = dict(os.environ, SOVEREIGN_NO_STALE_WARN="1")
    proc = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.exit(f"error: {' '.join(cmd)} failed:\n{proc.stderr.strip()}")
    return proc.stdout


def load_sessions(cwd, last, days):
    out = run_audit(["--json", "--sort", "recent", "--last", str(last)], cwd)
    cutoff = datetime.now(timezone.utc).timestamp() - days * 86400
    return [s for s in json.loads(out) if s.get("mtime_unix", 0) >= cutoff]


RAMP_ROW = re.compile(
    r"^(\w{8})\s+(\d+)\s+(\S+)\s+(\d+)t\s+(\d+)t\s+(\d+):(\d+)\s+(\d+)\s*$"
)


def load_ramp(cwd, last):
    """--ramp has no --json; parse the fixed-width table."""
    rows = {}
    for line in run_audit(["--ramp", "--sort", "recent", "--last", str(last)], cwd).splitlines():
        m = RAMP_ROW.match(line)
        if m:
            rows[m.group(1)] = {
                "reqs": int(m.group(2)),
                "first_edit_req": None if m.group(3) == "-" else m.group(3),
                "ramp_raw": int(m.group(4)),
                "ramp_intel": int(m.group(5)),
                "raw_calls": int(m.group(6)),
                "intel_calls": int(m.group(7)),
                "repeats": int(m.group(8)),
            }
    return rows


CF_HEADER = re.compile(
    r"actual total \$([\d,.]+) \(cache-read \$([\d,.]+)\)"
)
CF_LEVER = re.compile(
    r"^\s+(H\d\w*) (.+?)\s+\$\s*(-?[\d,.]+)\s+(-?[\d.]+)%\s*$"
)
CF_PREAMBLE = re.compile(r"avg (\d+)k tok")


def load_counterfactual(cwd, n_sessions):
    """--counterfactual has no --json; parse the lever lines."""
    text = run_audit(["--counterfactual", "--sort", "recent", "--last", str(max(n_sessions, 1))], cwd)
    fleet = {"levers": {}, "preamble_avg_ktok": None,
             "actual_total_usd": None, "cache_read_usd": None}
    m = CF_HEADER.search(text)
    if m:
        fleet["actual_total_usd"] = float(m.group(1).replace(",", ""))
        fleet["cache_read_usd"] = float(m.group(2).replace(",", ""))
    for line in text.splitlines():
        lm = CF_LEVER.match(line)
        if not lm:
            continue
        lever, desc, usd, pct = lm.groups()
        key = lever if lever != "H1" else "H1@" + (re.search(r"at (\d+k)", desc).group(1) if re.search(r"at (\d+k)", desc) else lever)
        fleet["levers"][key] = {"desc": desc.strip(), "usd": float(usd.replace(",", "")), "pct": float(pct)}
        if lever == "H2a":
            pm = CF_PREAMBLE.search(desc)
            if pm:
                fleet["preamble_avg_ktok"] = int(pm.group(1))
    return fleet


def load_split_events():
    events = {}
    if not os.path.exists(SPLIT_EVENTS):
        return events
    with open(SPLIT_EVENTS) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            events.setdefault(e["session_id"], []).append(e)
    return events


FRONTMATTER_KV = re.compile(r"^(\w+):\s*(.*)$")


def load_frame(session_id):
    path = os.path.join(SESSIONS_DIR, session_id, "frame.md")
    if not os.path.exists(path):
        return None
    meta = {}
    with open(path) as f:
        first = f.readline()
        if first.strip() != "---":
            return None
        for line in f:
            if line.strip() == "---":
                break
            m = FRONTMATTER_KV.match(line.strip())
            if m:
                meta[m.group(1)] = m.group(2)
    return meta


def parse_iso(ts):
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def count_commits(transcript_path):
    """Count distinct git-commit tool calls. Line-filter before JSON-parsing;
    dedupe by tool_use id (transcripts repeat content across lines)."""
    seen = set()
    try:
        with open(transcript_path, errors="replace") as f:
            for line in f:
                if "git commit" not in line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                content = (rec.get("message") or {}).get("content")
                if not isinstance(content, list):
                    continue
                for block in content:
                    if (isinstance(block, dict) and block.get("type") == "tool_use"
                            and "git commit" in json.dumps(block.get("input", {}))):
                        seen.add(block.get("id"))
    except OSError:
        return 0
    return len(seen)


def fmt_age(seconds):
    if seconds is None:
        return "-"
    if seconds < 3600:
        return f"{int(seconds // 60)}m"
    if seconds < 86400:
        return f"{seconds / 3600:.1f}h"
    return f"{seconds / 86400:.1f}d"


def fmt_usd(v):
    return f"${v:,.2f}"


def build_report(cwd, days, last):
    now = datetime.now(timezone.utc)
    sessions = load_sessions(cwd, last, days)
    if not sessions:
        sys.exit(f"no sessions in the last {days} days under {cwd}")
    ramp = load_ramp(cwd, last)
    fleet_cf = load_counterfactual(cwd, len(sessions))
    split_events = load_split_events()

    transcript_dir = None
    rows = []
    for s in sessions:
        sid_short = s["session"]
        sid_full = s["file"].removesuffix(".jsonl")
        # transcripts live next to whatever cache-audit read; recover the dir once
        if transcript_dir is None:
            candidate = os.path.expanduser(
                "~/.claude/projects/" + os.path.abspath(cwd).replace("/", "-"))
            transcript_dir = candidate if os.path.isdir(candidate) else None
        transcript = os.path.join(transcript_dir, s["file"]) if transcript_dir else None

        ev = [e for e in split_events.get(sid_full, [])]
        reds = [e for e in ev if e.get("level") == "red"]
        first_red = min((e["ts"] for e in reds), default=None)
        linger = (s["mtime_unix"] - first_red) if first_red else None

        frame = load_frame(sid_full)
        frame_end = parse_iso(frame.get("ended_at")) if frame else None
        # distill can stamp ended_at moments after the transcript's last write; clamp
        frame_age_at_end = max(0, s["mtime_unix"] - frame_end) if frame_end else None

        r = ramp.get(sid_short, {})
        ramp_raw = r.get("ramp_raw")
        ramp_pass = (ramp_raw is not None and ramp_raw <= RAMP_GATE_TOKENS
                     and r.get("repeats", 0) == 0)

        rows.append({
            "session": sid_short,
            "session_full": sid_full,
            "model": s.get("model", "?"),
            "turns": s.get("turns"),
            "peak_ctx": s.get("peak_ctx"),
            "cost_usd": s["cost_usd"]["total"],
            "cache_read_pct": s.get("cache_read_pct"),
            "raw_acq_tokens": s.get("raw_acq_tokens", 0),
            "code_intel_calls": s.get("code_intel_calls", 0),
            "ramp_raw": ramp_raw,
            "ramp_repeats": r.get("repeats"),
            "ramp_pass": ramp_pass if ramp_raw is not None else None,
            "red_crossings": len(reds),
            "red_linger_s": linger,
            "split_honored": (linger is not None and linger <= RED_HONOR_S) or None,
            "frame_provenance": frame.get("provenance") if frame else None,
            "frame_status": frame.get("status") if frame else None,
            "frame_age_at_end_s": frame_age_at_end,
            "commits": count_commits(transcript) if transcript and os.path.exists(transcript) else 0,
            "ended": datetime.fromtimestamp(s["mtime_unix"], timezone.utc).strftime("%m-%d %H:%M"),
        })

    rows.sort(key=lambda r: -r["cost_usd"])
    total_cost = sum(r["cost_usd"] for r in rows)
    total_commits = sum(r["commits"] for r in rows)
    total_reds = sum(r["red_crossings"] for r in rows)
    honored = sum(1 for r in rows if r["split_honored"])
    red_sessions = sum(1 for r in rows if r["red_crossings"] > 0)

    fleet = {
        "generated_at": now.isoformat(timespec="seconds"),
        "window_days": days,
        "sessions": len(rows),
        "total_cost_usd": round(total_cost, 2),
        "total_commits": total_commits,
        "red_crossings": total_reds,
        "red_sessions": red_sessions,
        "splits_honored": honored,
        "preamble_avg_ktok": fleet_cf["preamble_avg_ktok"],
        "levers": fleet_cf["levers"],
    }
    return rows, fleet


def load_previous_fleet(out_dir, today_tag):
    prevs = sorted(p for p in glob.glob(os.path.join(out_dir, "fleet-*.json"))
                   if today_tag not in os.path.basename(p))
    if not prevs:
        return None
    try:
        with open(prevs[-1]) as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return None


def render_markdown(rows, fleet, prev):
    lines = []
    lines.append(f"# Fleet report — {fleet['generated_at'][:10]} (last {fleet['window_days']}d)")
    lines.append("")
    lines.append(f"**{fleet['sessions']} sessions · {fmt_usd(fleet['total_cost_usd'])} · "
                 f"{fleet['total_commits']} commits · "
                 f"{fleet['red_crossings']} red crossings in {fleet['red_sessions']} sessions, "
                 f"{fleet['splits_honored']} splits honored (≤{RED_HONOR_S // 60}m linger)**")
    lines.append("")

    lines.append("## Per-session")
    lines.append("")
    lines.append("| session | model | cost | cache-rd % | raw acq tok | intel calls | ramp | red× | linger | frame @end | commits | ended (UTC) |")
    lines.append("|---|---|--:|--:|--:|--:|---|--:|--:|---|--:|---|")
    for r in rows:
        if r["ramp_pass"] is None:
            ramp_cell = "-"
        else:
            ramp_cell = (f"{'PASS' if r['ramp_pass'] else 'FAIL'} "
                         f"({r['ramp_raw']:,}t/{r['ramp_repeats']}rep)")
        frame_cell = "-"
        if r["frame_provenance"]:
            frame_cell = f"{r['frame_provenance']} ({fmt_age(r['frame_age_at_end_s'])})"
        lines.append(
            f"| {r['session']} | {r['model'].removeprefix('claude-')} | {fmt_usd(r['cost_usd'])} "
            f"| {r['cache_read_pct']:.0f} | {r['raw_acq_tokens']:,} | {r['code_intel_calls']} "
            f"| {ramp_cell} | {r['red_crossings']} | {fmt_age(r['red_linger_s'])} "
            f"| {frame_cell} | {r['commits']} | {r['ended']} |")
    lines.append("")
    lines.append(f"ramp = acquisition before first Edit/Write; gate ≤{RAMP_GATE_TOKENS // 1000}k raw + 0 repeated reads "
                 "(meaningful for frame-booted successors; upper bound for cold sessions). "
                 "frame @end = provenance (age of frame vs session end); self-reported frames "
                 "authorize splits, distilled are rescue-only.")
    lines.append("")

    lines.append("## Fleet levers (counterfactual, this window)")
    lines.append("")
    lines.append("| lever | size | share | trend vs prev report |")
    lines.append("|---|--:|--:|---|")
    prev_levers = (prev or {}).get("levers", {})
    for key, lv in fleet["levers"].items():
        trend = "-"
        if key in prev_levers:
            d = lv["pct"] - prev_levers[key]["pct"]
            trend = f"{d:+.1f}pp"
        lines.append(f"| {key} {lv['desc']} | {fmt_usd(lv['usd'])} | {lv['pct']:.1f}% | {trend} |")
    lines.append("")
    pre = fleet["preamble_avg_ktok"]
    if pre is not None:
        t = ""
        if prev and prev.get("preamble_avg_ktok"):
            t = f" (prev {prev['preamble_avg_ktok']}k)"
        lines.append(f"H2a preamble average: **{pre}k tokens/turn-0**{t} — the regime-change tracker; "
                     "MEMORY_MODEL predicts this falls as frames replace re-acquisition.")
    lines.append("")
    lines.append("Levers are independent counterfactuals and overlap — do not sum them.")
    lines.append("")
    if prev:
        lines.append(f"Previous report: {prev['generated_at'][:10]}, {prev['sessions']} sessions, "
                     f"{fmt_usd(prev['total_cost_usd'])}, {prev.get('red_crossings', 0)} red crossings, "
                     f"{prev.get('splits_honored', 0)} honored.")
        lines.append("")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description="Weekly fleet context-spend + split-adoption report")
    ap.add_argument("--project", default=os.getcwd(), help="project dir (default: cwd)")
    ap.add_argument("--days", type=int, default=7)
    ap.add_argument("--last", type=int, default=100, help="max sessions to consider before the date filter")
    ap.add_argument("--out-dir", default=DEFAULT_OUT)
    args = ap.parse_args()

    rows, fleet = build_report(args.project, args.days, args.last)
    os.makedirs(args.out_dir, exist_ok=True)
    tag = fleet["generated_at"][:10]
    prev = load_previous_fleet(args.out_dir, tag)
    md = render_markdown(rows, fleet, prev)

    md_path = os.path.join(args.out_dir, f"fleet-{tag}.md")
    json_path = os.path.join(args.out_dir, f"fleet-{tag}.json")
    with open(md_path, "w") as f:
        f.write(md + "\n")
    with open(json_path, "w") as f:
        json.dump(fleet, f, indent=2)

    print(md)
    print(f"\nwritten: {md_path}\n         {json_path}")


if __name__ == "__main__":
    main()
