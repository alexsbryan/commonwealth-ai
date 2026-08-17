#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The T1.5 acquisition ceiling probe (order deep-research-t1d).

Question the probe answers, verbatim from the order: "how many of the 72
v0 + 16 v1 keys are reachable if acquisition were perfect". Method
(declared in pre-registration.md BEFORE this probe ran, section
"Ceiling probe — declaration"):

- Perfect acquisition = the loop's window holds EVERY deck body, full
  text. (The window content cap is 12k chars per chunk; every deck body
  in both frozen banks is < 12k, so the cap is immaterial — journaled.)
- Per key, two reachability verdicts:

  1. CONTENT reachability — the scorer's evidence side
     (score-arms.py `score_keys`, the SAME deterministic semantics, no
     reimplemented threshold — §10.6): the deck can support the key's
     required figures and subjects.
       - journaled cannot-clear keys (K9): unreachable.
       - corrected figure keys (V1_CORRECTIONS require/subjects): every
         required figure `figure_present` in the concatenated bodies AND
         >=1 required subject present in the bodies (a subject absent
         from the deck is not nameable by any acquisition).
       - corrected figureless keys (K4): every required subject present,
         dot-normalized, in the concatenated bodies (the scorer's own
         normalization for corrected figureless keys).
       - base figureless keys (the v0 causal links): >=2 distinct
         subjects present in the bodies (the scorer's >=2 rule).
  2. FLOOR reachability — the corroboration floor (audit.rs
     CORROBORATION_FLOOR = 2, C-class distinct source_urls) run
     directly over the full deck: the distinct origins (deck hit URLs)
     whose bodies carry ANY of the key's required content. >=2 origins
     -> a claim about the key COULD pass the floor (the optimistic
     bound: the draft must actually cite both); else the floor caps the
     key forever, however acquisition runs. This is the honesty ceiling.

- Decision number (the order's gate): the v0 CONTENT ceiling vs 58.
  The floor ceiling is journaled separately — it is the honesty side
  (DEEP_RESEARCH.md P2: honesty never blended into coverage).

Deterministic, no model, no network. Reuses score-arms.py's extractors
verbatim (figures_of / figure_present / subjects_of / parse_v0_keys /
parse_v1_keys / V1_CORRECTIONS).

Output: the ceiling numbers + per-key rows (stdout), and the machine
record `ceiling-probe.json` beside this script.
"""

import importlib.util
import json
import sys
from pathlib import Path

import tomllib

# score-arms.py carries a dash in its filename, so it cannot be imported
# by name; load it from its path under a synthetic module id.
_arms = Path(__file__).parent
_spec = importlib.util.spec_from_file_location("score_arms", _arms / "score-arms.py")
_score_arms = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_score_arms)
sys.modules["score_arms"] = _score_arms
from score_arms import (  # noqa: E402  (the SAME extractors the scorer uses)
    V1_CORRECTIONS,
    figure_present,
    figures_of,
    parse_v0_keys,
    parse_v1_keys,
    subjects_of,
)

ARMS = Path(__file__).parent
BANK = ARMS.parent / "bank"
DECK_DIRS = sorted((ARMS / "decks").glob("seed-*"))
V1_DECK = BANK / "v1" / "deck"
V0_SEEDS = BANK / "seeds.md"
V1_SEEDS = BANK / "v1" / "seeds.md"


def load_deck(deck_dir):
    """deck.toml -> [{url, body}] (full body text — perfect acquisition)."""
    toml = tomllib.loads((deck_dir / "deck.toml").read_text())
    hits = []
    for h in toml["hit"]:
        body_path = deck_dir / h["body"]
        hits.append({"url": h["url"], "body": body_path.read_text()})
    return hits


def key_reachability(kid, key_text, hits, corr):
    """(content_reachable, floor_reachable, detail) — scorer semantics."""
    window = "\n".join(h["body"] for h in hits)
    norm_window = window.lower().replace(".", "")
    detail = {}

    if corr and corr.get("cannot_clear"):
        return False, False, {"reason": "journaled cannot-clear (arbiter journal)"}

    if corr and corr.get("figureless"):
        figs, subs, all_subs = [], corr.get("require_subjects", []), True
    else:
        figs = corr.get("require") if corr and "require" in corr else figures_of(key_text)
        subs = corr.get("subjects") if corr and "subjects" in corr else subjects_of(key_text, figs)
        all_subs = False

    missing_figs = [f for f in figs if not figure_present(f, window)]

    if all_subs:
        # corrected figureless: ALL required subjects, dot-normalized
        missing_subs = [s for s in subs if s not in norm_window]
        present_subs = [s for s in subs if s in norm_window]
        content = len(missing_figs) == 0 and len(missing_subs) == 0
        window_rule = "all-of subjects (dot-normalized), corrected figureless"
    elif figs:
        # figure keys: every figure in the deck; >=1 subject nameable
        present_subs = [s for s in subs if s in window.lower()]
        missing_subs = [] if present_subs else subs
        content = len(missing_figs) == 0 and bool(present_subs)
        window_rule = "all figures + >=1 subject in deck"
    else:
        # base figureless: >=2 distinct subjects in the deck
        present_subs = [s for s in subs if s in window.lower()]
        missing_subs = [] if len(present_subs) >= 2 else subs
        content = len(present_subs) >= 2
        window_rule = ">=2 subjects in deck (base figureless)"

    def carries(h):
        for f in figs:
            if figure_present(f, h["body"]):
                return True
        if all_subs:
            n = h["body"].lower().replace(".", "")
            return any(s in n for s in subs)
        return any(s in h["body"].lower() for s in subs)

    floor_origins = sorted({h["url"] for h in hits if carries(h)})
    floor = len(floor_origins) >= 2

    detail.update({
        "figures": [f for f in figs],
        "subjects": subs,
        "missing_figures": missing_figs,
        "missing_subjects": missing_subs,
        "window_rule": window_rule,
        "floor_origins": floor_origins,
        "origins_total": len({h["url"] for h in hits}),
    })
    return content, floor, detail


def probe_bank(seeds, deck_for, corrections, label):
    """seeds: parsed key rows {seed_id: [(kid, question, key_text)]};
    deck_for: seed_id -> deck dir; corrections: V1_CORRECTIONS or {}."""
    rows = []
    n_content = 0
    n_floor = 0
    for seed_id, key_rows in seeds.items():
        hits = load_deck(deck_for(seed_id))
        for kid, question, ktext in key_rows:
            content, floor, detail = key_reachability(kid, ktext, hits, corrections.get(kid))
            n_content += content
            n_floor += floor
            rows.append({
                "seed": seed_id, "key": kid, "content_reachable": content,
                "floor_reachable": floor, **detail,
            })
    return n_content, n_floor, rows


def main():
    v0 = parse_v0_keys(V0_SEEDS.read_text())
    v1 = parse_v1_keys(V1_SEEDS.read_text())
    v1_seeds = {"v1": v1}

    v0_content, v0_floor, v0_rows = probe_bank(
        v0, lambda sid: ARMS / "decks" / sid, {}, "v0")
    v1_content, v1_floor, v1_rows = probe_bank(
        v1_seeds, lambda sid: V1_DECK, V1_CORRECTIONS, "v1")

    n_v0 = sum(len(rows) for rows in v0.values())
    n_v1 = len(v1)
    assert n_v0 == 72, f"v0 keys parsed: {n_v0}, expected 72"
    assert n_v1 == 16, f"v1 keys parsed: {n_v1}, expected 16"

    record = {
        "method": "declared in pre-registration.md (Ceiling probe — declaration)",
        "v0": {
            "keys": n_v0,
            "content_ceiling": v0_content,
            "floor_ceiling": v0_floor,
            "unreachable": [
                {"seed": r["seed"], "key": r["key"],
                 "missing_figures": r["missing_figures"],
                 "missing_subjects": r["missing_subjects"],
                 "reason": r.get("reason")}
                for r in v0_rows if not r["content_reachable"]
            ],
            "rows": v0_rows,
        },
        "v1": {
            "keys": n_v1,
            "content_ceiling": v1_content,
            "floor_ceiling": v1_floor,
            "rows": v1_rows,
        },
    }
    out = ARMS / "ceiling-probe.json"
    out.write_text(json.dumps(record, indent=2, default=str) + "\n")

    print(f"v0: content ceiling {v0_content}/{n_v0}  floor ceiling {v0_floor}/{n_v0}")
    for r in v0_rows:
        if not r["content_reachable"]:
            print(f"  UNREACHABLE {r['seed']} {r['key']}: "
                  f"figs={r['missing_figures']} subs={r['missing_subjects']} {r.get('reason','')}")
    print(f"v1: content ceiling {v1_content}/{n_v1}  floor ceiling {v1_floor}/{n_v1}")
    for r in v1_rows:
        mark = "OK " if r["content_reachable"] else "NO "
        fmark = "2-org" if r["floor_reachable"] else "1-org"
        print(f"  {mark} {r['key']}: content={r['content_reachable']} floor={fmark} "
              f"figs={r.get('missing_figures')} subs={r.get('missing_subjects')} "
              f"{r.get('reason', '')}")
    print(f"decision: v0 content ceiling {v0_content} vs bar 58 -> "
          f"{'PROCEED to acquisition fixes' if v0_content >= 58 else 'ESCALATE (ceiling < 58)'}")
    print(f"record: {out}")


if __name__ == "__main__":
    main()
