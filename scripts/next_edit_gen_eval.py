#!/usr/bin/env python3
"""Next-edit MODEL-LANE eval (NEXT_EDIT.md §6, gym/next-edit/gen/README.md).

Runs gym/next-edit/gen/cases.jsonl against a live daemon's
/v1/edit_predictions with `model_lane: true` and scores the P2 lane.
Unlike the rule bank (a deterministic contract check), this scores a
model — but two of its gates are still deterministic code:

  GM1 structural   malformed model edits on the wire        == 0
  GM2 gate         consult decision + reason match, all     == 100%
  GM3 wrong-edit   wrong fires / all model fires            <= 5%
  GM4 usefulness   positives fired AND content-correct      >= 60%
  GM5 latency      wall p95                                 <= 6000 ms

GM3 is the default-on decider; GM1/GM2 red is a named bug (see the
bank README for verdict semantics). Reported, not gated: drop-reason
histogram, per-category breakdown, needle hit rate, partial fires.

Needs an editing model resident — but NOT a coder one. The next-edit
lane rides the model's ordinary prompt surface, so any chat model can
serve it, and the daemon falls back to the resident chat model when no
`[models.edit]` is configured. The runner probes first and names which
of those two situations it found.

Usage:
  python3 scripts/next_edit_gen_eval.py [--endpoint http://127.0.0.1:9741] \
      [--cases gym/next-edit/gen/cases.jsonl] [--kind positive|gate_negative|model_negative] \
      [--category casing_variant|...] [--limit N] [--json out.json] [-v]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request
from collections import Counter, defaultdict


def post(endpoint: str, request: dict, timeout: float) -> tuple[dict, float]:
    req = urllib.request.Request(
        f"{endpoint}/v1/edit_predictions",
        data=json.dumps(request).encode(),
        headers={"content-type": "application/json"},
    )
    t0 = time.monotonic()
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = json.loads(resp.read())
    return payload, (time.monotonic() - t0) * 1000


def apply_edits_u16(text: str, edits: list[dict]) -> str | None:
    """Splice edits (UTF-16 offsets) into text; None on overlap/bounds."""
    raw = text.encode("utf-16-le")
    total = len(raw) // 2
    out = bytearray()
    pos = 0
    for e in sorted(edits, key=lambda e: (e["start"], e["end"])):
        s, en = e["start"], e["end"]
        if not (isinstance(s, int) and isinstance(en, int) and 0 <= s <= en <= total):
            return None
        if s < pos:
            return None
        out += raw[2 * pos:2 * s]
        out += str(e["new_text"]).encode("utf-16-le")
        pos = en
    out += raw[2 * pos:]
    return out.decode("utf-16-le", errors="replace")


def cond_holds(doc: str, cond: dict) -> bool:
    if "count" in cond:
        s, n = cond["count"]
        return doc.count(s) == n
    if "count_ne" in cond:
        s, n = cond["count_ne"]
        return doc.count(s) != n
    if "recount" in cond:
        pat, n = cond["recount"]
        return len(re.findall(pat, doc)) == n
    if "contains" in cond:
        return cond["contains"] in doc
    if "not_contains" in cond:
        return cond["not_contains"] not in doc
    raise ValueError(f"unknown cond {cond}")


def run_case(endpoint: str, case: dict, timeout: float) -> dict:
    payload, wall_ms = post(endpoint, case["request"], timeout)
    expect = case["expect"]
    text = case["request"]["text"]
    edits = payload.get("edits", [])
    engine = payload.get("engine")
    dbg = payload.get("sovereign_debug", {})
    m = dbg.get("model") or {}

    r = {
        "id": case["id"], "kind": case["kind"],
        "category": case.get("category"), "language": case["language"],
        "engine": engine, "fired": engine == "model" and len(edits) > 0,
        "gate_ok": True, "malformed": [], "wrong": False, "correct": False,
        "dropped": m.get("dropped"), "needle_hit": m.get("needle_hit"),
        "wall_ms": round(wall_ms, 1),
    }

    # ---- GM2: the deterministic gate --------------------------------
    r["gate_ok"] = m.get("consulted") == expect["consult"]
    if r["gate_ok"] and expect["consult"] and expect.get("consult_reason"):
        r["gate_ok"] = m.get("reason") == expect["consult_reason"]
    if r["gate_ok"] and not expect["consult"] and expect.get("not_consulted_reason"):
        r["gate_ok"] = m.get("skipped") == expect["not_consulted_reason"]

    # Rule-owns ordering case: rule fires, model stays out. Nothing
    # model-structural to score.
    if expect.get("engine") == "rule":
        if engine != "rule" or not edits:
            r["gate_ok"] = False
        return r

    if engine == "model" and edits:
        # ---- GM1: structural ----------------------------------------
        total = len(text.encode("utf-16-le")) // 2
        region = m.get("region") or {}
        prev_end = None
        for e in sorted(edits, key=lambda e: (e.get("start", -1), e.get("end", -1))):
            s, en = e.get("start"), e.get("end")
            if not (isinstance(s, int) and isinstance(en, int) and 0 <= s <= en <= total):
                r["malformed"].append(f"bad offsets {s}..{en}")
                continue
            if prev_end is not None and s < prev_end:
                r["malformed"].append(f"overlap at {s}")
            prev_end = en
            if region and not (region["start"] <= s and en <= region["end"]):
                r["malformed"].append(f"edit {s}..{en} outside reported region")
        doc_after = apply_edits_u16(text, edits)
        if doc_after is None:
            r["malformed"].append("edits do not splice")

        # ---- GM3 / GM4: content -------------------------------------
        if case["kind"] == "model_negative":
            r["wrong"] = True
        elif doc_after is not None:
            r["wrong"] = any(cond_holds(doc_after, c) for c in expect.get("wrong", []))
            r["correct"] = not r["wrong"] and all(
                cond_holds(doc_after, c) for c in expect.get("correct", []))
    return r


def pct(vals: list[float], p: float) -> float:
    vals = sorted(vals)
    if not vals:
        return -1
    return vals[max(0, min(len(vals) - 1, round(p * (len(vals) - 1))))]


def probe_model(endpoint: str, timeout: float) -> None:
    """Fail fast, with the fix, when the next-edit lane is not served."""
    request = {
        "history": [
            {"before": "", "after": ", tmo", "left": "dial(a, x", "right": ")"},
            {"before": "", "after": ", tmo", "left": "dial(b, y", "right": ")"},
        ],
        "text": "dial(a, x, tmo)\ndial(b, y, tmo)\ndial(c, z)\n",
        "cursor": 0, "debug": True, "model_lane": True,
    }
    payload, _ = post(endpoint, request, timeout)
    m = (payload.get("sovereign_debug") or {}).get("model") or {}
    if m.get("dropped") == "unavailable":
        sys.exit(
            "model lane unavailable: the daemon's editing slot serves no "
            "next-edit lane.\n"
            "Set [models.edit] in ~/.svrnmesh/config.toml (see "
            "INLINE_COMPLETION.md §3.5), `sovereign daemon restart`, re-run.\n"
            "Any competent chat model serves this lane — it does NOT need a "
            "coder GGUF — so a daemon with a resident chat model and no "
            "[models.edit] should be falling back rather than reporting this.")
    if not m:
        sys.exit("daemon predates the model lane: sovereign_debug.model missing — "
                 "rebuild + redeploy the daemon before scoring this bank.")
    # Name the instrument before reporting a number (ARCH §18.4). A
    # degraded arrangement scores the resident CHAT model, not a chosen
    # edit model — a real result, but attributing it to the wrong model
    # is how a bank silently measures something else.
    describe_edit_slot(endpoint, timeout)


def describe_edit_slot(endpoint: str, timeout: float) -> None:
    """Print which model + lane the daemon will actually score with.

    Best-effort: /status is glassbox, not a gate, so a probe failure
    prints what it knows and lets the run proceed.
    """
    try:
        with urllib.request.urlopen(f"{endpoint}/status", timeout=timeout) as resp:
            inf = (json.loads(resp.read()) or {}).get("inference") or {}
    except Exception as e:  # noqa: BLE001 — never block the bank on a status read
        print(f"edit slot: unknown (/status probe failed: {e})")
        return
    # `edit` is the current key; `fim` is the deprecated byte-identical
    # mirror and the only key a pre-two-lane daemon emits.
    slot = inf.get("edit") or inf.get("fim")
    if not slot:
        print("edit slot: /status reports none (the lane probe above passed, "
              "so this daemon predates inference.edit)")
        return
    print(f"edit slot: {slot.get('model_id')} on '{slot.get('slot')}' "
          f"next_edit={slot.get('next_edit_format', '<none>')} "
          f"fim={slot.get('fim_style', '<none>')} "
          f"degraded={slot.get('degraded', False)}")
    if slot.get("degraded"):
        print("  WARNING: no [models.edit] configured — this bank is scoring the "
              "resident chat model, not a chosen edit model.")
    if slot.get("advice"):
        print(f"  daemon advice: {slot['advice']}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--endpoint", default="http://127.0.0.1:9741")
    ap.add_argument("--cases", default="gym/next-edit/gen/cases.jsonl")
    ap.add_argument("--kind", default=None)
    ap.add_argument("--category", default=None)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=float, default=90.0)
    ap.add_argument("--json", default=None)
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    cases = [json.loads(l) for l in open(args.cases, encoding="utf-8") if l.strip()]
    if args.kind:
        cases = [c for c in cases if c["kind"] == args.kind]
    if args.category:
        cases = [c for c in cases if c.get("category") == args.category]
    if args.limit:
        cases = cases[:args.limit]
    if not cases:
        sys.exit("no cases after filtering")

    try:
        probe_model(args.endpoint, args.timeout)
    except urllib.error.URLError as e:
        sys.exit(f"daemon unreachable at {args.endpoint}: {e}")

    results = []
    for i, c in enumerate(cases, 1):
        try:
            r = run_case(args.endpoint, c, args.timeout)
        except Exception as e:  # noqa: BLE001 — one bad case shouldn't kill the bank
            r = {"id": c["id"], "kind": c["kind"], "category": c.get("category"),
                 "language": c["language"], "engine": None, "fired": False,
                 "gate_ok": False, "malformed": [f"error: {e}"], "wrong": False,
                 "correct": False, "dropped": None, "needle_hit": None, "wall_ms": -1}
        results.append(r)
        ok = (r["gate_ok"] and not r["malformed"] and not r["wrong"]
              and (r["correct"] or not (c["kind"] == "positive" and c["expect"]["fire"])))
        if args.verbose or not ok:
            mark = "✓" if ok else "✗"
            why = "" if ok else \
                f"  [gate_ok={r['gate_ok']} fired={r['fired']} wrong={r['wrong']} " \
                f"correct={r['correct']} dropped={r['dropped']} " \
                f"malformed={r['malformed'][:2]}]"
            print(f"[{i:>2}/{len(cases)}] {mark} {c['kind']:15s} {c['id']}{why}")

    groups = defaultdict(list)
    for r in results:
        groups[r.get("category") or r["kind"]].append(r)
        groups["ALL"].append(r)
    print(f"\n{'group':18s} {'n':>3} {'gate_ok':>8} {'fired':>6} {'correct':>8} "
          f"{'wrong':>6} {'malf':>5} {'wall50':>8} {'wall95':>8}")
    for g, rs in sorted(groups.items()):
        n = len(rs)
        walls = [r["wall_ms"] for r in rs if r["wall_ms"] >= 0]
        print(f"{g:18s} {n:>3} {sum(r['gate_ok'] for r in rs) / n:>8.2f} "
              f"{sum(r['fired'] for r in rs):>6} {sum(r['correct'] for r in rs):>8} "
              f"{sum(r['wrong'] for r in rs):>6} "
              f"{sum(len(r['malformed']) for r in rs):>5} "
              f"{pct(walls, .5):>8.0f} {pct(walls, .95):>8.0f}")

    drops = Counter(r["dropped"] for r in results if r["dropped"])
    if drops:
        print(f"\ndrop reasons: {dict(drops)}")
    consulted = [r for r in results if r["needle_hit"] is not None]
    if consulted:
        hits = sum(1 for r in consulted if r["needle_hit"])
        print(f"needle hit rate: {hits}/{len(consulted)}")
    positives = [r for r in results if r["kind"] == "positive"]
    partial = sum(1 for r in positives if r["fired"] and not r["wrong"] and not r["correct"])
    if partial:
        print(f"partial fires (not wrong, not fully correct): {partial}")

    # ---- gates ------------------------------------------------------
    malformed_total = sum(len(r["malformed"]) for r in results)
    gm1 = malformed_total == 0
    gm2_n = sum(r["gate_ok"] for r in results)
    gm2 = gm2_n == len(results)
    fires = [r for r in results if r["fired"]]
    wrong_fires = sum(r["wrong"] for r in fires)
    gm3 = (wrong_fires / len(fires) <= 0.05) if fires else True
    gm4_n = sum(r["correct"] for r in positives)
    gm4 = (gm4_n / len(positives) >= 0.60) if positives else True
    walls = [r["wall_ms"] for r in results if r["wall_ms"] >= 0]
    gm5 = pct(walls, .95) <= 6000 if walls else False

    print(f"\ngates: GM1 structural {'PASS' if gm1 else 'FAIL'} (malformed={malformed_total})"
          f" · GM2 gate {'PASS' if gm2 else 'FAIL'} ({gm2_n}/{len(results)})"
          f" · GM3 wrong-edit {'PASS' if gm3 else 'FAIL'}"
          f" ({wrong_fires}/{len(fires) if fires else 0} fires)"
          f" · GM4 usefulness {'PASS' if gm4 else 'FAIL'} ({gm4_n}/{len(positives)})"
          f" · GM5 latency {'PASS' if gm5 else 'FAIL'} (p95={pct(walls, .95):.0f}ms)")
    verdict = gm1 and gm2 and gm3 and gm4 and gm5
    print(f"default-on verdict: {'GREEN — GM3+GM4 clear' if gm3 and gm4 and gm1 and gm2 else 'stay opt-in'}")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(results, fh, indent=2)
        print(f"raw results → {args.json}")
    sys.exit(0 if verdict else 1)


if __name__ == "__main__":
    main()
