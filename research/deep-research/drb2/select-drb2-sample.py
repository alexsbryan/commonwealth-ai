#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""DRB-II sample selection (order deep-research-t7a, pre-registration §3).

Content-blind, stratified, seeded:
  - Population: the 64 English tasks (66 en minus NC-licensed idx 26 and
    110; CC0 idx 119 is CC-licensed, included).
  - Strata: the 22 themes, weighted inverse to Perplexity's per-theme
    totals (paper arXiv 2601.08536 v2, Appendix B Table 8 — the named
    substitution for the order's 'domains where Perplexity's InfoRecall
    was weakest', which are not published per-domain).
  - Seed: sha256("deep-research-t7a-drb2-sample-2026-08-19")[:8] -> int.
  - Draw: 8 tasks without replacement; per draw, theme ~ weight, then a
    task uniformly from that theme's eligible tasks.

CONTENT-BLIND: this script reads ONLY idx, language, theme, license from
tasks_and_rubrics.jsonl. Prompts/rubrics are never opened; the flight
driver reads them at flight time. The selection.json output pins the seed
string, weights, and draws (the audit key).

Weights are pre-registered (Table 8, verbatim):
  Art & Design 0.4292, Crime & Law 0.4861, Education & Jobs 0.3304,
  Entertainment 0.2957, Fashion & Beauty 0.2042, Finance & Business
  0.4145, Food & Dining 0.3927, Games 0.2766, Hardware 0.3494, Health
  0.4635, History 0.4183, Home & Hobbies 0.5311, Industrial 0.3320,
  Literature 0.3810, Religion 0.3587, Science & Technology 0.3727,
  Social Life 0.2866, Software 0.5523, Software Development 0.3819,
  Sports & Fitness 0.3216, Transportation 0.3692, Travel 0.4231.
Weight per theme = round(1000/score)/1000 (weaker theme -> larger weight).
"""

import argparse
import hashlib
import json
import random
import sys
from pathlib import Path

SEED_STRING = "deep-research-t7a-drb2-sample-2026-08-19"
N_DRAWS = 8
NC_LICENSES = ("cc by-nc", "cc-by-nc")

TABLE8_SCORES = {
    "Art & Design": 0.4292,
    "Crime & Law": 0.4861,
    "Education & Jobs": 0.3304,
    "Entertainment": 0.2957,
    "Fashion & Beauty": 0.2042,
    "Finance & Business": 0.4145,
    "Food & Dining": 0.3927,
    "Games": 0.2766,
    "Hardware": 0.3494,
    "Health": 0.4635,
    "History": 0.4183,
    "Home & Hobbies": 0.5311,
    "Industrial": 0.3320,
    "Literature": 0.3810,
    "Religion": 0.3587,
    "Science & Technology": 0.3727,
    "Social Life": 0.2866,
    "Software": 0.5523,
    "Software Development": 0.3819,
    "Sports & Fitness": 0.3216,
    "Transportation": 0.3692,
    "Travel": 0.4231,
}


def load_tasks(path: str):
    """Reads ONLY idx, language, theme, license (content-blind)."""
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            obj = json.loads(line)
            rows.append({
                "idx": int(obj["idx"]),
                "language": obj.get("language", ""),
                "theme": obj.get("theme", ""),
                "license": obj.get("license", ""),
            })
    return rows


def eligible(tasks):
    """English, non-NC tasks. License check is substring on lowercase."""
    out = []
    for t in tasks:
        if t["language"] != "en":
            continue
        lic = t["license"].lower()
        if any(nc in lic for nc in NC_LICENSES):
            continue
        out.append(t)
    return out


def main():
    ap = argparse.ArgumentParser(description="DRB-II content-blind sample (t7a)")
    ap.add_argument("--tasks", default="/home/alexbryan/dev/DeepResearch-Bench-II/tasks_and_rubrics.jsonl")
    ap.add_argument("--out", default=str(Path(__file__).parent / "selection.json"))
    ap.add_argument("--n", type=int, default=N_DRAWS)
    args = ap.parse_args()

    rows = load_tasks(args.tasks)
    pool = eligible(rows)
    print(f"[info] tasks total={len(rows)} eligible(en, non-NC)={len(pool)}")

    # theme validation: every Table 8 key must exist in the data's themes
    data_themes = sorted({t["theme"] for t in pool})
    missing = [k for k in TABLE8_SCORES if k not in data_themes]
    if missing:
        print(f"[err] Table 8 themes missing from data: {missing}")
        print(f"[info] data themes: {data_themes}")
        sys.exit(1)
    unknown = [t for t in data_themes if t not in TABLE8_SCORES]
    if unknown:
        print(f"[warn] data themes with no Table 8 score (weight 0, "
              f"never drawn): {unknown}")

    weights = {k: round(1000.0 / v) / 1000.0 for k, v in TABLE8_SCORES.items()}
    wsum = sum(weights.values())
    probs = {k: v / wsum for k, v in weights.items()}

    seed = int(hashlib.sha256(SEED_STRING.encode()).hexdigest()[:8], 16)
    rng = random.Random(seed)
    print(f"[info] seed_string={SEED_STRING!r} seed={seed}")

    by_theme = {}
    for t in pool:
        by_theme.setdefault(t["theme"], []).append(t)

    draws = []
    seen = set()
    for d in range(args.n):
        theme = rng.choices(list(weights.keys()), weights=list(weights.values()))[0]
        cands = [t for t in by_theme.get(theme, []) if t["idx"] not in seen]
        if not cands:
            # theme exhausted (unlikely at n=8); redraw from remaining pool
            cands = [t for t in pool if t["idx"] not in seen]
        t = rng.choice(cands)
        seen.add(t["idx"])
        draws.append({
            "draw": d + 1,
            "idx": t["idx"],
            "theme": t["theme"],
            "language": t["language"],
            "license": t["license"],
            "theme_weight": weights[t["theme"]],
        })
        print(f"[draw {d+1}] idx={t['idx']:>3} theme={t['theme']:<24} "
              f"weight={weights[t['theme']]:.3f} license={t['license']}")

    selection = {
        "seed_string": SEED_STRING,
        "seed": seed,
        "n": args.n,
        "weights": {k: round(v, 4) for k, v in weights.items()},
        "theme_probabilities": {k: round(v, 4) for k, v in probs.items()},
        "exclusion": {"rule": "language==en AND license not CC BY-NC",
                      "excluded_nc_idx": [26, 110],
                      "note": "NC tasks excluded per t6g control-arm design; "
                              "CC0 idx 119 included (not NC)"},
        "stratum_source": ("paper arXiv 2601.08536 v2 Appendix B Table 8 "
                           "(Perplexity per-theme totals) — the named "
                           "substitution for per-domain InfoRecall weakness, "
                           "which is not published"),
        "draws": draws,
        "content_blind": ("selection read only idx/language/theme/license; "
                          "prompts opened first at flight time"),
    }
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(selection, indent=2, ensure_ascii=False) + "\n",
                              encoding="utf-8")
    print(f"[done] selection written to {args.out}")


if __name__ == "__main__":
    main()
