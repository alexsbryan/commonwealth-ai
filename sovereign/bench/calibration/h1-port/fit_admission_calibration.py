#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Derive the H1 admission stage's runtime calibration from the FROZEN
kill-gate scores.

The kill gate (NATIVE_GROUNDING.md §7.3, settled 582382e1) proved the
rerank margin separates answerable from absent at 0.8990 AUROC. It did
NOT hand the runtime an operating point, and it did not hand it a way
to turn a model-dependent logit into the `answerability: f32` in 0..1
that `GroundingVerdict` declares. This script derives both, once,
deterministically, from `h1_scores.jsonl` — the same 4,207 pairs the
verdict was computed on, byte-identical (sha256 asserted below).

Two outputs, both committed:

  * **Platt coefficients** (a, b) for `answerability = sigmoid(a*m + b)`.
    Fitted by full-batch gradient descent with a fixed step count and no
    randomness, so re-running reproduces the coefficients bit-for-bit.
    This is the ONE implementation of the margin->probability map;
    `admission.rs` reads the coefficients from this artifact's JSON and
    has no second formula (ARCH §10.6).

  * **Two thresholds** in answerability space, read off the frozen
    curve rather than invented:
      - `tau_abstain` = the margin at which 5% of genuinely ANSWERABLE
        pairs fall below (the 5% false-alarm budget FINDINGS.md names as
        "the operating point a production router would actually want").
        Below it: Abstain. This is the bar that protects D5's
        competence-when-present, which is why it is the conservative one.
      - `tau_answer` = the best-balanced-accuracy threshold the frozen
        curve already carries (6.6807...). At or above it: Answer.
      - Between them: Hedge — proceed, but born Parametric-typed under
        the structural GK caveat.

Refuses rather than substitutes: if the frozen scores are not the file
the kill gate scored, it exits non-zero instead of fitting on whatever
is there (ARCH §18.3).

Usage:
    python3 fit_admission_calibration.py <h1_scores.jsonl> <curve.json> <out.json>
"""

import hashlib
import json
import math
import sys

# The kill-gate score file this calibration is fitted on. Pinned so a
# silent swap of the input is a refusal, not a different calibration.
EXPECTED_SCORES_SHA256 = (
    "594adad6f8e4a1098991f72d2f6637f72cc1fcfd3ef962a80cdd4e163d099963"
)
EXPECTED_CURVE_SHA256 = (
    "eb1b00657d8571e96d32cd323e8b608544a8a9cabddd13cdff676564ca2116ac"
)

# Fixed-schedule full-batch gradient descent. No RNG, no shuffling, no
# early stop — the same inputs give the same coefficients on any host.
LR = 0.05
STEPS = 20000
# The false-alarm budget tau_abstain is read at. FINDINGS.md §"The AUROC
# gap understates the practical one" names 5% as the production point.
FALSE_ALARM_BUDGET = 0.05


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(65536), b""):
            h.update(block)
    return h.hexdigest()


def sigmoid(z):
    # Numerically stable both ways.
    if z >= 0:
        return 1.0 / (1.0 + math.exp(-z))
    e = math.exp(z)
    return e / (1.0 + e)


def fit_platt(margins, labels):
    """Logistic regression of P(answerable) on the raw margin.

    Standardizes the margin first so one fixed learning rate works
    regardless of the reranker's logit scale, then folds the
    standardization back into (a, b) so the emitted coefficients apply
    to the RAW margin — the runtime never has to know a mean or a
    standard deviation.
    """
    n = len(margins)
    mean = sum(margins) / n
    var = sum((m - mean) ** 2 for m in margins) / n
    sd = math.sqrt(var) if var > 0 else 1.0
    xs = [(m - mean) / sd for m in margins]

    w = 0.0
    b = 0.0
    for _ in range(STEPS):
        gw = 0.0
        gb = 0.0
        for x, y in zip(xs, labels):
            p = sigmoid(w * x + b)
            d = p - y
            gw += d * x
            gb += d
        w -= LR * gw / n
        b -= LR * gb / n

    # sigmoid(w*(m-mean)/sd + b) == sigmoid(a*m + c)
    a = w / sd
    c = b - w * mean / sd
    return a, c


def logloss(margins, labels, a, c):
    total = 0.0
    for m, y in zip(margins, labels):
        p = min(max(sigmoid(a * m + c), 1e-12), 1 - 1e-12)
        total += -(y * math.log(p) + (1 - y) * math.log(1 - p))
    return total / len(margins)


def brier(margins, labels, a, c):
    return sum((sigmoid(a * m + c) - y) ** 2 for m, y in zip(margins, labels)) / len(
        margins
    )


def main():
    if len(sys.argv) != 4:
        print(__doc__)
        return 2
    scores_path, curve_path, out_path = sys.argv[1:4]

    got = sha256(scores_path)
    if got != EXPECTED_SCORES_SHA256:
        print(
            f"REFUSING: {scores_path} sha256 {got} != the kill-gate score file "
            f"{EXPECTED_SCORES_SHA256}. This calibration is only meaningful on "
            f"the pairs the H1 verdict was computed on.",
            file=sys.stderr,
        )
        return 3
    got_curve = sha256(curve_path)
    if got_curve != EXPECTED_CURVE_SHA256:
        print(
            f"REFUSING: {curve_path} sha256 {got_curve} != the kill-gate curve "
            f"{EXPECTED_CURVE_SHA256}.",
            file=sys.stderr,
        )
        return 3

    margins, labels = [], []
    with open(scores_path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            margins.append(float(r["rerank_margin"]))
            labels.append(1.0 if r["answerable"] else 0.0)

    curve = json.load(open(curve_path))
    assert curve["signal"] == "rerank_margin", curve["signal"]
    assert len(margins) == curve["n_pairs"], (len(margins), curve["n_pairs"])

    a, c = fit_platt(margins, labels)

    # tau_abstain: the LOWEST threshold on the frozen curve whose false
    # alarm is still within budget, i.e. the most honesty we can buy for
    # 5% of wrongly-refused answerable turns. Points are ascending in
    # threshold, and false_alarm is monotone non-decreasing in it.
    pts = curve["points"]
    within = [p for p in pts if p["false_alarm"] <= FALSE_ALARM_BUDGET]
    if not within:
        print("REFUSING: no curve point within the false-alarm budget", file=sys.stderr)
        return 4
    abstain_pt = max(within, key=lambda p: p["threshold"])
    tau_abstain_margin = abstain_pt["threshold"]
    tau_answer_margin = curve["best_balanced_accuracy_threshold"]
    if not tau_answer_margin > tau_abstain_margin:
        print(
            f"REFUSING: degenerate band — abstain {tau_abstain_margin} >= "
            f"answer {tau_answer_margin}",
            file=sys.stderr,
        )
        return 5

    tau_abstain = sigmoid(a * tau_abstain_margin + c)
    tau_answer = sigmoid(a * tau_answer_margin + c)

    # What the band actually costs, on the calibration set. Reported,
    # not asserted: D5 measures the runtime consequence.
    n_ans = sum(1 for y in labels if y == 1.0)
    n_abs = len(labels) - n_ans
    regions = {"answer": [0, 0], "hedge": [0, 0], "abstain": [0, 0]}
    for m, y in zip(margins, labels):
        r = (
            "answer"
            if m >= tau_answer_margin
            else ("abstain" if m < tau_abstain_margin else "hedge")
        )
        regions[r][0 if y == 1.0 else 1] += 1

    out = {
        "schema": "h1-admission-calibration/v1",
        "derived_from": {
            "scores": "sovereign/bench/calibration/h1-port/h1_scores.jsonl",
            "scores_sha256": EXPECTED_SCORES_SHA256,
            "curve": "sovereign/bench/calibration/h1-port/h1_rerank_margin.overall.curve.json",
            "curve_sha256": EXPECTED_CURVE_SHA256,
            "kill_gate_note": "582382e1",
            "auroc": curve["auroc"],
        },
        "platt": {
            "form": "answerability = 1 / (1 + exp(-(a * rerank_margin + b)))",
            "a": a,
            "b": c,
            "fit": {
                "method": "full-batch gradient descent, fixed schedule, no RNG",
                "lr": LR,
                "steps": STEPS,
                "log_loss": logloss(margins, labels, a, c),
                "brier": brier(margins, labels, a, c),
            },
        },
        "thresholds": {
            "false_alarm_budget": FALSE_ALARM_BUDGET,
            "tau_abstain_margin": tau_abstain_margin,
            "tau_answer_margin": tau_answer_margin,
            "tau_abstain": tau_abstain,
            "tau_answer": tau_answer,
            "at_tau_abstain": {
                "honesty_recall": abstain_pt["honesty_recall"],
                "false_alarm": abstain_pt["false_alarm"],
                "balanced_accuracy": abstain_pt["balanced_accuracy"],
            },
            "at_tau_answer": {
                "balanced_accuracy": curve["best_balanced_accuracy"],
            },
        },
        "region_occupancy_on_calibration_set": {
            "n_answerable": n_ans,
            "n_absent": n_abs,
            "answer": {"answerable": regions["answer"][0], "absent": regions["answer"][1]},
            "hedge": {"answerable": regions["hedge"][0], "absent": regions["hedge"][1]},
            "abstain": {
                "answerable": regions["abstain"][0],
                "absent": regions["abstain"][1],
            },
        },
    }
    with open(out_path, "w") as fh:
        json.dump(out, fh, indent=2, sort_keys=True)
        fh.write("\n")
    print(json.dumps(out["thresholds"], indent=2))
    print(json.dumps(out["region_occupancy_on_calibration_set"], indent=2))
    print(f"platt a={a!r} b={c!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
