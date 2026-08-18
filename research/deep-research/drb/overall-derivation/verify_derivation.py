#!/usr/bin/env python3
"""T5a worked-derivation verifier (phases 1-2).

Reproduces every number in the T5A measurement-path inventory from official
artifacts only, and asserts them. Exit 0 = every assertion held; non-zero =
something did not reproduce (do not proceed to phase 3 on a failure).

Reads (all read-only):
  - the upstream clone (DRB_UPSTREAM_CLONE, default
    /home/alexbryan/dev/deep_research_bench) @ 469cce54
  - the vendored leaderboard.csv (frozen, sha256 dd184970...)
  - the vendored fixture + vendored utils (for the stat reproduction)
  - ./inputs/ — official per-task artifacts fetched from the leaderboard
    space (sha256 pinned in INPUTS.md)

Derivations:
  D-A  aggregation reproduction on the repo-shipped race data
  D-B  leaderboard row 39 end-to-end (all 7 numbers)
  D-C  perplexity subset-10 references (both judge eras)
  D-D  structural: leaderboard overall is not a function of its dim columns
  D-E  FACT stats reproduction (vendored stat.py on the vendored fixture)
  D-F  named non-reproductions (discrepancies, asserted as documented facts)
"""
import csv
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
DRB = HERE.parent  # research/deep-research/drb
REPO = DRB.parent.parent.parent  # commonwealth-ai
CLONE = Path(os.environ.get("DRB_UPSTREAM_CLONE", "/home/alexbryan/dev/deep_research_bench"))
PIN = "469cce54ea7f6a63c163d3d9fec879cf289ec484"
SUBSET_IDS = [56, 58, 59, 62, 65, 69, 78, 83, 90, 95]

failures = []


def check(name, cond, detail=""):
    tag = "PASSED" if cond else "FAILED"
    print(f"  [{tag}] {name}" + (f"  ({detail})" if detail else ""))
    if not cond:
        failures.append(name)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def mean(xs):
    xs = list(xs)
    return sum(xs) / len(xs)


def main():
    print("== T5a worked-derivation verifier ==")
    print(f"  clone: {CLONE}")
    print(f"  drb:   {DRB}")

    # ---- guards: clone present at the pinned commit ----
    head = subprocess.run(["git", "-C", str(CLONE), "rev-parse", "HEAD"],
                          capture_output=True, text=True).stdout.strip()
    check("clone at pinned commit", head == PIN, head[:12])

    lb_path = DRB / "leaderboard.csv"
    check("vendored leaderboard sha256 (dd184970...)", sha256(lb_path).startswith("dd184970"),
          sha256(lb_path)[:12])
    rows = list(csv.DictReader(open(lb_path)))
    check("leaderboard has 45 data rows", len(rows) == 45, str(len(rows)))

    # ---------------- D-A: shipped race data aggregation ----------------
    print("\nD-A  aggregation reproduction (repo-shipped claude-3-7-sonnet-latest)")
    race_dir = CLONE / "results" / "race" / "claude-3-7-sonnet-latest"
    raw = [json.loads(l) for l in open(race_dir / "raw_results.jsonl")]
    check("D-A rows=100, no errors", len(raw) == 100 and all("error" not in r for r in raw))
    official = {}
    for line in open(race_dir / "race_result.txt"):
        k, v = line.strip().split(":", 1)
        official[k.strip()] = float(v.strip())
    dim_map = {"Comprehensiveness": "comprehensiveness", "Insight": "insight",
               "Instruction Following": "instruction_following", "Readability": "readability",
               "Overall Score": "overall_score"}
    for ok, dk in dim_map.items():
        m = mean(r[dk] for r in raw)
        check(f"D-A {dk}: mean={m:.4f} == official {ok}={official[ok]:.4f}",
              abs(m - official[ok]) < 5e-5)

    # ---------------- D-B: leaderboard row 39 end-to-end ----------------
    print("\nD-B  reference row 39 (perplexity-Research) end-to-end")
    inputs = HERE / "inputs"
    raw39 = [json.loads(l) for l in open(inputs / "perplexity-raw_results.jsonl")]
    check("D-B rows=100, ids 1..100",
          len(raw39) == 100 and sorted(r["id"] for r in raw39) == list(range(1, 101)))
    check("D-B fetched file sha256 = LFS oid (1141aa12...)", sha256(inputs / "perplexity-raw_results.jsonl").startswith("1141aa12"))
    lb39 = [r for r in rows if r["model"] == "perplexity-Research"][0]
    check("D-B row exists", lb39["overall_score"] == "40.46")
    for dk in ["comprehensiveness", "insight", "instruction_following", "readability", "overall_score"]:
        m = mean(r[dk] for r in raw39) * 100
        lbv = float(lb39[dk])
        check(f"D-B {dk}: mean*100={m:.4f} vs leaderboard {lbv:.2f}", abs(m - lbv) < 0.005)
    fact = dict(l.strip().split(":", 1) for l in open(inputs / "perplexity-fact_result.txt"))
    acc = float(fact["valid_rate"]) * 100
    eff = float(fact["total_valid_citations"])
    check(f"D-B citation_accuracy: {acc:.4f} vs leaderboard {lb39['citation_accuracy']}", abs(acc - float(lb39["citation_accuracy"])) < 0.005)
    check(f"D-B effective_citations: {eff:.4f} vs leaderboard {lb39['effective_citations']}", abs(eff - float(lb39["effective_citations"])) < 0.005)

    # ---------------- D-C: subset-10 references ----------------
    print("\nD-C  perplexity on OUR 10 frozen subset tasks (like-for-like references)")
    sub = [r for r in raw39 if r["id"] in SUBSET_IDS]
    check("D-C 10 subset rows found", len(sub) == 10, str(sorted(r["id"] for r in sub)))
    g_overall = mean(r["overall_score"] for r in sub) * 100
    check("D-C gemini-era subset overall = 42.1779", abs(g_overall - 42.1779) < 1e-3, f"{g_overall:.4f}")
    raw55 = [json.loads(l) for l in open(inputs / "perplexity-gpt55-raw_results.jsonl")]
    sub55 = [r for r in raw55 if r["id"] in SUBSET_IDS]
    check("D-C gpt55 10 subset rows found", len(sub55) == 10)
    g55_overall = mean(r["overall_score"] for r in sub55) * 100
    check("D-C gpt55-era subset overall = 44.9683", abs(g55_overall - 44.9683) < 1e-3, f"{g55_overall:.4f}")
    check("D-C gpt55 full-100 = 43.0516", abs(mean(r["overall_score"] for r in raw55) * 100 - 43.0516) < 1e-3)
    check("D-C gemini full-100 = 40.4581", abs(mean(r["overall_score"] for r in raw39) * 100 - 40.4581) < 1e-3)

    print("\nD-C  criteria readiness (shipped criteria.jsonl rows for the 10 subset tasks)")
    cri = {c["id"]: c for c in (json.loads(l) for l in open(CLONE / "data/criteria_data/criteria.jsonl"))}
    dw_bad, cw_bad = [], []
    for i in SUBSET_IDS:
        if i not in cri:
            cw_bad.append(f"id {i}: row missing")
            continue
        c = cri[i]
        if abs(sum(c["dimension_weight"].values()) - 1.0) > 1e-9:
            dw_bad.append(i)
        for dim, crits in c["criterions"].items():
            if abs(sum(x["weight"] for x in crits) - 1.0) > 1e-9:
                cw_bad.append(f"{i}:{dim}")
    check("D-C criteria: all 10 rows present, dimension_weight sums to 1.0",
          not dw_bad, str(dw_bad))
    check("D-C criteria: per-dim criterion-weight sums to 1.0 in all rows",
          not cw_bad, str(cw_bad))

    # ---------------- D-AB: A/B arm readiness (decided flight, operator resolve 2026-08-17) ----
    print("\nD-AB  same-judge same-task A/B arm readiness (perplexity's 10 official subset articles)")
    ab = inputs / "perplexity-subset-articles.jsonl"
    check("D-AB subset-articles sha256 = b1ce5783...", sha256(ab).startswith("b1ce5783"), sha256(ab)[:12])
    ab_rows = [json.loads(l) for l in open(ab)]
    check("D-AB 10 rows, ids = subset ids",
          len(ab_rows) == 10 and sorted(r["id"] for r in ab_rows) == SUBSET_IDS)
    sub_q = [json.loads(l) for l in open(DRB / "query.subset.jsonl")]
    qmap = {q["id"]: q["prompt"] for q in sub_q}
    check("D-AB prompts match the frozen query.subset.jsonl",
          all(r["prompt"] == qmap[r["id"]] for r in ab_rows))
    check("D-AB all articles substantive (len > 5000)",
          all(len(r["article"]) > 5000 for r in ab_rows))
    check("D-AB full-100 raw file sha256 = 0a3b8558 (both judge eras identical)",
          sha256(inputs / "perplexity-raw_data.jsonl").startswith("0a3b8558"))

    # ---------------- D-D: overall is not a column function ----------------
    print("\nD-D  structural: leaderboard overall != mean of dim columns")
    ndiff = 0
    for r in rows:
        d4 = [float(r[k]) for k in ["comprehensiveness", "insight", "instruction_following", "readability"]]
        if abs(mean(d4) - float(r["overall_score"])) > 0.01:
            ndiff += 1
    check("D-D 45/45 rows: |mean4 - overall| > 0.01", ndiff == 45, f"{ndiff}/45")

    # ---------------- D-E: FACT stats reproduction ----------------
    print("\nD-E  vendored stat.py on vendored fixture -> official fact_result.txt")
    vendor = DRB / "vendor"
    out = HERE / "fact_repro.txt"
    r = subprocess.run(
        ["python3", "-m", "utils.stat", "--input_path", "fixture-validated.jsonl",
         "--output_path", str(out)],
        cwd=str(vendor), capture_output=True, text=True)
    check("D-E stat.py exit 0", r.returncode == 0, r.stderr[-200:] if r.returncode else "")
    official_fact = (CLONE / "results" / "fact" / "claude-3-7-sonnet-latest" / "fact_result.txt").read_text()
    check("D-E byte-identical to official fact_result.txt",
          out.exists() and out.read_text() == official_fact)

    # ---------------- D-F: named non-reproductions ----------------
    print("\nD-F  named non-reproductions (documented, not reconciled by assumption)")
    shipped_race = official["Overall Score"] * 100
    lb_claude = [r for r in rows if r["model"] == "claude-3-7-sonnet-with-search"][0]
    check("D-F shipped claude run (42.18) != leaderboard row (36.63) — named discrepancy",
          abs(shipped_race - float(lb_claude["overall_score"])) > 0.5,
          f"shipped {shipped_race:.2f} vs leaderboard {lb_claude['overall_score']}")
    check("D-F claude citation dims DO reproduce (87.32 / 24.51)",
          abs(float(lb_claude["citation_accuracy"]) - 87.32) < 0.005
          and abs(float(lb_claude["effective_citations"]) - 24.51) < 0.005)
    # old-space rows not present in the new space's per-task dirs (auth-gated): assert the
    # six rows exist in the vendored CSV but no per-task dir exists for them in the space listing
    old_space = {"grok-deeper-search", "sonar-reasoning-pro", "sonar-reasoning",
                 "claude-3-7-sonnet-with-search", "sonar-pro", "gpt-4o-search-preview"}
    lb_models = {r["model"] for r in rows}
    check("D-F six old-space rows present in vendored CSV", old_space <= lb_models)
    # Their per-task dirs are absent from the new space's data/raw_results (43 dirs listed
    # 2026-08-17); the old space is auth-gated. Documented in T5A_MEASUREMENT_PATH.md §7.3.

    # ---------------- verdict ----------------
    print(f"\n== verdict: {'ALL ASSERTIONS HELD' if not failures else 'FAILED: ' + '; '.join(failures)} ==")
    sys.exit(0 if not failures else 1)


if __name__ == "__main__":
    main()
