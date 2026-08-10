#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""D5 — the bar verdict for the per-corpus tau A/B.

Bars are pre-registered in seat-logged directive aca4639f
(operator-approved 2026-08-10), quoted verbatim in the docstrings below.
This script computes nothing it cannot source; a bar it cannot measure is
`could_not_judge` with the reason (ARCH §18.3), and the red-line numbers
are read from the bench's own report lines, never recomputed (one decider).

    (i)   competence-when-present >= 0.71 on BOTH on-runs
    (ii)  honesty-when-absent >= 0.91 on both
    (iii) abstains on the 31 answerable probes <= 2
    (iv)  admission decisions identical across the 2 on-runs
    KILL-CLAUSE: if FA<=5% on compound forces tau' below every compound
    margin, per-corpus thresholding is vacuous — tuning-failed.

Usage: d5_verdict.py <step3_dir> <out.json>
Expects: d5_on_r1.run.log, d5_on_r2.run.log, d5_off_reval.run.log,
         percorpus_tau_saltgrass.json, and the saltgrass bank for the
         qtype map.
"""
import json, re, sys
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-9;]*m")
REDLINE = re.compile(
    r"RED-LINE (\d+)\s+([a-z-]+)\s*:\s*([\d.]+)\s*\(([^)]*)\)\s*(PASS|FAIL)\s*\[([^\]]*)\]"
)
ABSENT_QTYPES = {"absent_adjacent", "absent_out_of_domain"}

def bank_qtypes():
    bank = Path(__file__).resolve().parents[2] / "chaos_monkey/saltgrass.toml"
    import tomllib
    qs = tomllib.loads(bank.read_text())["questions"]
    return {q["question"]: q["qtype"] for q in qs}

def redlines(log_text):
    out = {}
    for m in REDLINE.finditer(ANSI.sub("", log_text)):
        out[m.group(2)] = {"value": float(m.group(3)), "bench_bar": m.group(4).strip(),
                           "bench_verdict": m.group(5), "detail": m.group(6).strip()}
    return out

def admissions(log_text):
    out, cur_q = [], None
    for line in log_text.splitlines():
        line = ANSI.sub("", line)
        m = re.search(r'\[router\] "([^"]+)"', line)
        if m:
            cur_q = m.group(1)
            continue
        if "native-grounding H1: answerability admission" in line:
            g = lambda k: re.search(k + r"=(\S+)", line)
            out.append({"q_prefix": cur_q,
                        "decision": g("decision").group(1),
                        "margin": float(g("margin").group(1)),
                        "tau_source": (g("tau_source") or g("decided_by")).group(1)})
    return out

def main():
    d = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parent)
    outp = Path(sys.argv[2]) if len(sys.argv) > 2 else d / "d5_verdict.json"
    logs = {}
    for arm in ("on_r1", "on_r2", "off_reval"):
        p = d / f"d5_{arm}.run.log"
        logs[arm] = p.read_text() if p.is_file() else None

    fit = json.loads((d / "percorpus_tau_saltgrass.json").read_text())
    qt = bank_qtypes()

    verdict = {"schema": "step3-d5-verdict/v1",
               "bars_registered": "seat directive aca4639f, 2026-08-10, pre-run",
               "fit": {"env": fit["env"], "n_compound_turns": fit["n_compound_turns"],
                       "abstains_at_tau": fit["abstains_at_tau"],
                       "tau_abstain_margin": fit["fitted"]["tau_abstain_margin"]},
               "bars": {}}

    # KILL-CLAUSE check (from the fit artifact, computable pre-run).
    vacuous = fit["fitted"]["tau_abstain_margin"] <= min(fit["compound_margins"])
    verdict["kill_clause"] = {"vacuous": vacuous}

    # (i)/(ii): the bench's own red-lines per arm.
    rl = {arm: (redlines(t) if t else None) for arm, t in logs.items()}
    def bar_over_arms(name, key, bar):
        vals = {}
        for arm in ("on_r1", "on_r2"):
            if rl[arm] is None or key not in (rl[arm] or {}):
                return {"verdict": "could_not_judge", "why": f"{arm} log missing or lacks RED-LINE {key}"}
            vals[arm] = rl[arm][key]["value"]
        ok = all(v >= bar for v in vals.values())
        off = rl["off_reval"][key]["value"] if rl.get("off_reval") and key in (rl["off_reval"] or {}) else None
        return {"verdict": "passed" if ok else "failed", "bar": bar, "on_r1": vals["on_r1"],
                "on_r2": vals["on_r2"], "off_reval": off}
    verdict["bars"]["i_competence_when_present"] = bar_over_arms("competence", "competence-when-present", 0.71)
    verdict["bars"]["ii_honesty_when_absent"] = bar_over_arms("honesty", "honesty-when-absent", 0.91)

    # (iii): abstains among answerable (competence-set) probes, per on-run.
    def abstain_count(text):
        n = 0
        for a in admissions(text):
            if a["decision"] != "Abstain" or not a["q_prefix"]:
                continue
            qtype = next((t for q, t in qt.items() if q.startswith(a["q_prefix"])), None)
            if qtype is not None and qtype not in ABSENT_QTYPES:
                n += 1
        return n
    if logs["on_r1"] and logs["on_r2"]:
        c1, c2 = abstain_count(logs["on_r1"]), abstain_count(logs["on_r2"])
        verdict["bars"]["iii_answerable_abstains_max2"] = {
            "verdict": "passed" if max(c1, c2) <= 2 else "failed", "bar": 2, "on_r1": c1, "on_r2": c2}
    else:
        verdict["bars"]["iii_answerable_abstains_max2"] = {"verdict": "could_not_judge", "why": "missing on-run log"}

    # (iv): identical admission decisions across the two on-runs.
    if logs["on_r1"] and logs["on_r2"]:
        a1 = [(a["q_prefix"], a["decision"]) for a in admissions(logs["on_r1"])]
        a2 = [(a["q_prefix"], a["decision"]) for a in admissions(logs["on_r2"])]
        verdict["bars"]["iv_admission_identical"] = {
            "verdict": "passed" if a1 == a2 else "failed",
            "n_r1": len(a1), "n_r2": len(a2),
            "diff": [x for x in a1 if x not in a2][:5] + [x for x in a2 if x not in a1][:5]}
    else:
        verdict["bars"]["iv_admission_identical"] = {"verdict": "could_not_judge", "why": "missing on-run log"}

    # tau_source sanity: every on-run admission must say env_override. A
    # dead override (env never reached the process) must yield
    # could_not_judge, NEVER "failed" — a tuning judged failed when it
    # never ran is the §18.4 instrument error, in the direction the bars
    # themselves cannot catch (they would report the Step-2 signature and
    # look like a real failure).
    override_live = True
    for arm in ("on_r1", "on_r2"):
        if logs[arm]:
            srcs = {a["tau_source"] for a in admissions(logs[arm])}
            verdict.setdefault("instrument", {})[f"{arm}_tau_sources"] = sorted(srcs)
            if srcs != {"env_override"}:
                override_live = False
        else:
            override_live = False
    verdict.setdefault("instrument", {})["override_live_on_both_on_runs"] = override_live

    overall = ("tuning-failed(kill-clause)" if vacuous else
               "could_not_judge(override-not-live)" if not override_live else
               "passed" if all(b.get("verdict") == "passed" for b in verdict["bars"].values())
               else "failed" if any(b.get("verdict") == "failed" for b in verdict["bars"].values())
               else "could_not_judge")
    verdict["overall"] = overall
    outp.write_text(json.dumps(verdict, indent=2) + "\n")
    print(json.dumps(verdict["bars"], indent=1))
    print("OVERALL:", overall)
    return 0 if overall == "passed" else 1

if __name__ == "__main__":
    sys.exit(main())
