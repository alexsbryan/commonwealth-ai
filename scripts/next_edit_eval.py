#!/usr/bin/env python3
"""Next-edit rule-lane eval (NEXT_EDIT.md §6, gym/next-edit/README.md).

Runs gym/next-edit/cases.jsonl against a live daemon's
/v1/edit_predictions and reports, per case-kind and overall:

  fire_ok     fired exactly when the case expects
  queue_ok    authored: returned queue == expected, ordered;
              harvest-pos: every held-out commit site in the queue
  malformed   returned edits violating the rule-lane contract
              (bad offsets, overlap, old span ≠ rule find,
              new_text ≠ rule replace)
  over-offer  queue sites beyond the held-out commit sites
              (reported, NOT gated — designed queue semantics)
  wall ms     request wall time

Gates (pre-registered in gym/next-edit/README.md — do not move):
  G1 malformed == 0 and authored queue_ok == 100%
  G2 harvest-pos fire_ok == 100% and held-out recall == 100%
  G3 all negatives silent == 100% (authored reasons exact)
  G4 wall p95 <= 150 ms
  G5 every annotated decline still declines by its named mechanism

G1 and G2 read the UNANNOTATED population; G5 reads the annotated one.
That split exists because two mechanisms landed AFTER these fixtures
were cut and decline some of them BY DESIGN — the syntax oracle
(`next_edit_syntax.rs`, 2026-08-06: −56% junk hunks for −12% good ones)
and the MIN_RULE_CHARS 4→5 sweep (2026-08-07, which routes short rules
to the pair lane). Scored against the original bar those cases can only
ever fail, so G2 could only ever be red and carried no signal at all:
a team could not tell an inherited red from a regression they caused.

A case declined by a designed trade carries `expect.declined_by`, and
that annotation is a CHECK, not a waiver — G5 re-derives the mechanism
every run and fails if it no longer describes what the daemon does:

  min_rule_chars  the daemon itself reports `below_threshold`. The
                  threshold is NOT re-derived here; the daemon is the
                  one decider (`next_edit.rs::MIN_RULE_CHARS`) and this
                  reads its verdict rather than duplicating the value.
  pair_fallback   a DIFFERENT, anchored rule fired, and every held-out
                  site is text-equivalently covered by one of its edits
                  — so the routed edit makes the change the fixture
                  wanted, at a wider anchor. An unrelated edit fails.
  syntax_oracle   every held-out site the run withheld REAPPEARS when
                  the request carries a path no grammar matches. That
                  counterfactual is what distinguishes "the oracle
                  filtered it" from "site finding is broken".

So an annotation cannot paper over a regression: if the engine stops
declining for the stated reason, G5 goes red. If an annotated case
starts passing outright, G5 also goes red and says to delete the
annotation, so the waiver set cannot rot into a silent green.

Exit code 0 iff all gates pass. No model weights needed — the rule
lane is pure string work on any daemon build with the route.

Usage:
  python3 scripts/next_edit_eval.py [--endpoint http://127.0.0.1:9741] \
      [--cases gym/next-edit/cases.jsonl] [--kind authored|harvest-pos|harvest-neg] \
      [--limit N] [--json out.json] [-v]
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from collections import defaultdict


def u16len(s: str) -> int:
    return sum(2 if ord(c) > 0xFFFF else 1 for c in s)


def u16_slice(text_u16: bytes, start: int, end: int) -> str:
    return text_u16[2 * start:2 * end].decode("utf-16-le", errors="replace")


def post(endpoint: str, request: dict, timeout: float) -> dict:
    req = urllib.request.Request(
        f"{endpoint}/v1/edit_predictions",
        data=json.dumps(request).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())


def without_grammar(endpoint: str, case: dict, timeout: float) -> set:
    """The same request under a path no grammar matches, so the syntax
    oracle declines ("cannot judge") and returns sites untouched. The
    counterfactual behind the `syntax_oracle` annotation: it separates
    "the oracle withheld this site" from "site finding is broken"."""
    req = dict(case["request"])
    req["path"] = "buffer.txt"
    req["language"] = "plaintext"
    p = post(endpoint, req, timeout)
    return {(e.get("start"), e.get("end")) for e in p.get("edits", [])}


def verify_decline(mech, r, dbg, got, missed, text_u16, endpoint, case, timeout):
    """Re-derive the annotated mechanism against THIS run. Returns
    (ok, detail). See the module docstring for why each predicate is
    the honest one."""
    if mech == "min_rule_chars":
        # The daemon owns the threshold and reports its own verdict; we
        # read it rather than re-deriving MIN_RULE_CHARS on this side
        # (one decider, one name).
        ok = dbg.get("reason_silent") == "below_threshold"
        return ok, f"reason={dbg.get('reason_silent')!r}"
    if mech == "pair_fallback":
        if not got:
            return False, "nothing fired"
        if dbg.get("rule_find") == case["expect"].get("rule_find"):
            return False, "the predicted rule fired — this is not a routed case"
        for w in missed:
            host = [g for g in got if g[0] <= w[0] and g[1] >= w[1]]
            if not host:
                return False, f"held-out site {w[0]}..{w[1]} not covered by the routed edit"
            g = host[0]
            rebuilt = u16_slice(text_u16, g[0], w[0]) + w[2] + u16_slice(text_u16, w[1], g[1])
            if rebuilt != g[2]:
                return False, f"routed edit at {g[0]}..{g[1]} is not text-equivalent to {w[0]}..{w[1]}"
        return True, f"routed to {dbg.get('rule_find')!r}, text-equivalent"
    if mech == "syntax_oracle":
        if not missed:
            return False, "nothing was withheld"
        loose = without_grammar(endpoint, case, timeout)
        absent = [w for w in missed if (w[0], w[1]) not in loose]
        if absent:
            return False, (f"{len(absent)} held-out site(s) still missing with no "
                           f"grammar — not the oracle")
        return True, f"{len(missed)} site(s) withheld, all reappear with no grammar"
    return False, f"unknown mechanism {mech!r}"


def _score_case(endpoint: str, case: dict, timeout: float):
    t0 = time.monotonic()
    payload = post(endpoint, case["request"], timeout)
    wall_ms = (time.monotonic() - t0) * 1000

    expect = case["expect"]
    text = case["request"]["text"]
    text_u16 = text.encode("utf-16-le")
    total_u16 = len(text_u16) // 2
    edits = payload.get("edits", [])
    dbg = payload.get("sovereign_debug", {})
    fired = len(edits) > 0

    r = {
        "id": case["id"], "kind": case["kind"], "language": case["language"],
        "fired": fired, "fire_ok": fired == expect["fire"],
        "queue_ok": True, "reason_ok": True, "malformed": [],
        "recall_missed": 0, "over_offer": 0, "wall_ms": round(wall_ms, 1),
        "reason": dbg.get("reason_silent"),
        "declined_by": expect.get("declined_by"),
        "decline_ok": None, "decline_stale": False, "decline_detail": "",
    }
    ctx = {"dbg": dbg, "text_u16": text_u16, "got": [], "missed": []}

    if not expect["fire"]:
        if fired:
            r["queue_ok"] = False
        elif "reasons" in expect and case["kind"] == "authored":
            r["reason_ok"] = dbg.get("reason_silent") in expect["reasons"]
        return r, ctx

    if not fired:
        r["queue_ok"] = False
        r["recall_missed"] = len(expect.get("sites", []))
        ctx["missed"] = [(w["start"], w["end"], w["new_text"])
                         for w in expect.get("sites", [])]
        return r, ctx

    # Structural well-formedness (G1) is a claim about the rule that
    # ACTUALLY fired, not the one this fixture predicted. `should_fire`
    # is a router: a `find` under MIN_RULE_CHARS is declined and the
    # case falls through to the insertion/deletion lane, which
    # re-induces a longer, line-anchored rule from the same history.
    # Those edits are perfectly well-formed under the rule that fired.
    # Scoring them against `expect["rule_find"]` reported them as
    # malformed and charged a routed case to the correctness gate.
    # Whether the rule was the one we wanted is a QUEUE question, and
    # `queue_ok`/`recall_missed` below already answer it.
    find = dbg.get("rule_find") or expect["rule_find"]
    replace = dbg.get("rule_replace") if dbg.get("rule_find") else None
    if replace is None:
        replace = expect["rule_replace"]
    flen = u16len(find)
    got = []
    prev_sorted_end = None
    for e in edits:
        s, en, nt = e.get("start"), e.get("end"), e.get("new_text")
        if not (isinstance(s, int) and isinstance(en, int)
                and 0 <= s <= en <= total_u16):
            r["malformed"].append(f"bad offsets {s}..{en}")
            continue
        if en - s != flen or u16_slice(text_u16, s, en) != find:
            r["malformed"].append(f"old span at {s} != rule find")
        if nt != replace:
            r["malformed"].append(f"new_text at {s} != rule replace")
        got.append((s, en, nt))
    for s, en, _ in sorted(got):
        if prev_sorted_end is not None and s < prev_sorted_end:
            r["malformed"].append(f"overlap at {s}")
        prev_sorted_end = en

    want = [(w["start"], w["end"], w["new_text"]) for w in expect.get("sites", [])]
    got_set = set(got)
    ctx["got"] = got
    ctx["missed"] = [w for w in want if w not in got_set]
    if expect.get("exact"):
        r["queue_ok"] = got == want
        if expect.get("expect_capped") and not dbg.get("edits_capped", False):
            r["malformed"].append("expected edits_capped=true")
    else:
        r["recall_missed"] = len(ctx["missed"])
        r["queue_ok"] = r["recall_missed"] == 0
        r["over_offer"] = len(got) - (len(want) - r["recall_missed"])
    return r, ctx


def run_case(endpoint: str, case: dict, timeout: float) -> dict:
    """Score the case, then — if it carries a `declined_by` annotation —
    re-derive that mechanism against this very run (G5)."""
    r, ctx = _score_case(endpoint, case, timeout)
    mech = r["declined_by"]
    if not mech:
        return r
    passed = (r["fire_ok"] and r["queue_ok"] and r["reason_ok"] and not r["malformed"])
    if passed:
        # The trade that declined this case no longer does. That is good
        # news and still a red gate: a waiver nobody prunes is how a bank
        # rots into a green that means nothing.
        r["decline_stale"] = True
        r["decline_ok"] = False
        r["decline_detail"] = f"case now PASSES — delete declined_by={mech!r} from the fixture"
        return r
    r["decline_ok"], r["decline_detail"] = verify_decline(
        mech, r, ctx["dbg"], ctx["got"], ctx["missed"], ctx["text_u16"],
        endpoint, case, timeout)
    return r


def pct(vals: list[float], p: float) -> float:
    vals = sorted(vals)
    if not vals:
        return -1
    return vals[max(0, min(len(vals) - 1, round(p * (len(vals) - 1))))]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--endpoint", default="http://127.0.0.1:9741")
    ap.add_argument("--cases", default="gym/next-edit/cases.jsonl")
    ap.add_argument("--kind", default=None)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--timeout", type=float, default=30.0)
    ap.add_argument("--json", default=None)
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    cases = [json.loads(l) for l in open(args.cases, encoding="utf-8") if l.strip()]
    if args.kind:
        cases = [c for c in cases if c["kind"] == args.kind]
    if args.limit:
        cases = cases[:args.limit]
    if not cases:
        sys.exit("no cases after filtering")

    results = []
    for i, c in enumerate(cases, 1):
        try:
            r = run_case(args.endpoint, c, args.timeout)
        except urllib.error.URLError as e:
            sys.exit(f"daemon unreachable at {args.endpoint}: {e}. "
                     "The rule lane needs no model — any daemon build with the "
                     "route serves this bank.")
        except Exception as e:  # noqa: BLE001 — one bad case shouldn't kill the bank
            r = {"id": c["id"], "kind": c["kind"], "language": c["language"],
                 "fired": None, "fire_ok": False, "queue_ok": False,
                 "reason_ok": False, "malformed": [f"error: {e}"],
                 "recall_missed": 0, "over_offer": 0, "wall_ms": -1, "reason": None,
                 "declined_by": c["expect"].get("declined_by"), "decline_ok": False,
                 "decline_stale": False, "decline_detail": f"error: {e}"}
        results.append(r)
        ok = r["fire_ok"] and r["queue_ok"] and r["reason_ok"] and not r["malformed"]
        if r["declined_by"]:
            # Three verdicts, not two: a decline the annotation still
            # explains is neither a pass nor a failure.
            mark, why = ("·", f"  [declined_by={r['declined_by']}: {r['decline_detail']}]") \
                if r["decline_ok"] else \
                ("✗", f"  [declined_by={r['declined_by']} NO LONGER HOLDS: {r['decline_detail']}]")
            if args.verbose or not r["decline_ok"]:
                print(f"[{i:>3}/{len(cases)}] {mark} {c['kind']:12s} {c['id'][:70]}{why}")
        elif args.verbose or not ok:
            mark = "✓" if ok else "✗"
            why = "" if ok else \
                f"  [fire_ok={r['fire_ok']} queue_ok={r['queue_ok']} " \
                f"reason={r['reason']} missed={r['recall_missed']} " \
                f"malformed={r['malformed'][:2]}]"
            print(f"[{i:>3}/{len(cases)}] {mark} {c['kind']:12s} {c['id'][:70]}{why}")

    groups = defaultdict(list)
    for r in results:
        groups[r["kind"]].append(r)
        groups["ALL"].append(r)
    print(f"\n{'kind':14s} {'n':>4} {'fire_ok':>8} {'queue_ok':>9} "
          f"{'malformed':>10} {'overoffer50':>12} {'wall50':>7} {'wall95':>7}")
    for kind, rs in sorted(groups.items()):
        n = len(rs)
        print(f"{kind:14s} {n:>4} {sum(r['fire_ok'] for r in rs) / n:>8.2f} "
              f"{sum(r['queue_ok'] for r in rs) / n:>9.2f} "
              f"{sum(len(r['malformed']) for r in rs):>10} "
              f"{pct([r['over_offer'] for r in rs], .5):>12.0f} "
              f"{pct([r['wall_ms'] for r in rs if r['wall_ms'] >= 0], .5):>7.1f} "
              f"{pct([r['wall_ms'] for r in rs if r['wall_ms'] >= 0], .95):>7.1f}")

    # G1/G2 read the population that is supposed to work; G5 reads the
    # one two designed trades decline. Mixing them is what made G2 a
    # gate that could only ever fail (see the module docstring).
    live = [r for r in results if not r["declined_by"]]
    declined = [r for r in results if r["declined_by"]]
    authored = [r for r in live if r["kind"] == "authored"]
    hpos = [r for r in live if r["kind"] == "harvest-pos"]
    negs = [r for r in live if not next(c for c in cases if c["id"] == r["id"])["expect"]["fire"]]
    malformed_total = sum(len(r["malformed"]) for r in results)
    g1 = malformed_total == 0 and all(
        r["queue_ok"] for r in authored if next(
            c for c in cases if c["id"] == r["id"])["expect"]["fire"])
    g2 = all(r["fire_ok"] and r["queue_ok"] for r in hpos) if hpos else True
    g3 = all(r["fire_ok"] and r["reason_ok"] for r in negs) if negs else True
    walls = [r["wall_ms"] for r in results if r["wall_ms"] >= 0]
    g4 = pct(walls, .95) <= 150 if walls else False
    g5 = all(r["decline_ok"] for r in declined) if declined else True

    # G1 has two arms; printing only the malformed count made a run that
    # failed on the authored-queue arm read as "FAIL (malformed=0)".
    authored_fire = [r for r in authored if next(
        c for c in cases if c["id"] == r["id"])["expect"]["fire"]]
    a_ok = sum(r["queue_ok"] for r in authored_fire)
    by_mech = defaultdict(lambda: [0, 0])
    for r in declined:
        by_mech[r["declined_by"]][0] += 1
        by_mech[r["declined_by"]][1] += bool(r["decline_ok"])
    print(f"\ngates: G1 correctness {'PASS' if g1 else 'FAIL'}"
          f" (malformed={malformed_total}, authored queue {a_ok}/{len(authored_fire)})"
          f" · G2 contract-recall {'PASS' if g2 else 'FAIL'}"
          f" ({sum(r['fire_ok'] and r['queue_ok'] for r in hpos)}/{len(hpos)})"
          f" · G3 restraint {'PASS' if g3 else 'FAIL'}"
          f" ({sum(r['fire_ok'] and r['reason_ok'] for r in negs)}/{len(negs)})"
          f" · G4 latency {'PASS' if g4 else 'FAIL'} (p95={pct(walls, .95):.1f}ms)"
          f" · G5 declines {'PASS' if g5 else 'FAIL'}"
          f" ({sum(bool(r['decline_ok']) for r in declined)}/{len(declined)})")
    if declined:
        print(f"  {len(live)} of {len(results)} cases scored; {len(declined)} declined by a "
              f"designed trade, each re-verified this run:")
        for mech, (n, ok) in sorted(by_mech.items()):
            stale = sum(1 for r in declined if r["declined_by"] == mech and r["decline_stale"])
            note = "" if ok == n else f"  <- {n - ok} NO LONGER HOLD" + (
                f" ({stale} now pass outright — prune the annotation)" if stale else "")
            print(f"    {mech:16s} {ok:>3}/{n:<3} still declining{note}")

    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(results, fh, indent=2)
        print(f"raw results → {args.json}")
    sys.exit(0 if (g1 and g2 and g3 and g4 and g5) else 1)


if __name__ == "__main__":
    main()
