# SPDX-License-Identifier: AGPL-3.0-or-later
"""Assertions for the tax. Every claim the README makes about the degenerate
strategy is pinned here instead of asserted in prose (ARCH §7.2).

Run: python3 test_score.py
"""
from score import (
    DEFAULT_TAX, OFFICIAL_TAX, accuracy, break_even_confidence,
    hallucination_rate, omniscience_index, oracle_ceiling,
)

# Claude 4.1 Opus at the AA snapshot: index 4.8, accuracy 36% (arXiv 2511.13029).
# Back out the histogram: c=36.0%, c-i=4.8% => i=31.2%, remainder abstained.
OPUS = {"CORRECT": 360, "INCORRECT": 312, "NOT_ATTEMPTED": 328, "PARTIAL_ANSWER": 0}
ABSTAIN_ALL = {"CORRECT": 0, "INCORRECT": 0, "NOT_ATTEMPTED": 1000, "PARTIAL_ANSWER": 0}
WRONG_ALL = {"CORRECT": 0, "INCORRECT": 1000, "NOT_ATTEMPTED": 0, "PARTIAL_ANSWER": 0}
RIGHT_ALL = {"CORRECT": 1000, "INCORRECT": 0, "NOT_ATTEMPTED": 0, "PARTIAL_ANSWER": 0}

def close(a, b, eps=1e-9):
    assert abs(a - b) < eps, f"{a} != {b}"

def test_tax_zero_is_the_official_metric():
    close(omniscience_index(OPUS, OFFICIAL_TAX), 4.8)
    close(omniscience_index(ABSTAIN_ALL, OFFICIAL_TAX), 0.0)
    close(accuracy(OPUS), 0.36)

def test_official_metric_pays_for_blanket_abstention():
    """The defect we are correcting: declining everything scores 0, which beat
    all but three models at the Nov-2025 snapshot."""
    assert omniscience_index(ABSTAIN_ALL, OFFICIAL_TAX) == 0.0
    assert omniscience_index(ABSTAIN_ALL, OFFICIAL_TAX) > -100.0

def test_tax_makes_blanket_abstention_stop_paying():
    close(omniscience_index(ABSTAIN_ALL, DEFAULT_TAX), -10.0)
    close(omniscience_index(OPUS, DEFAULT_TAX), 1.52)
    assert omniscience_index(ABSTAIN_ALL, DEFAULT_TAX) < omniscience_index(OPUS, DEFAULT_TAX)

def test_tax_does_not_make_guessing_profitable():
    """The tax must stay MILD: converting an abstention into a wrong answer has
    to remain a loss at every tax we would consider shipping."""
    for tax in (0.0, 0.05, 0.1, 0.25, 0.5):
        base = omniscience_index(ABSTAIN_ALL, tax)
        guessed_wrong = dict(ABSTAIN_ALL, NOT_ATTEMPTED=999, INCORRECT=1)
        guessed_right = dict(ABSTAIN_ALL, NOT_ATTEMPTED=999, CORRECT=1)
        assert omniscience_index(guessed_wrong, tax) < base, tax
        assert omniscience_index(guessed_right, tax) > base, tax

def test_tax_is_a_shift_in_the_confidence_threshold():
    close(break_even_confidence(OFFICIAL_TAX), 0.50)
    close(break_even_confidence(DEFAULT_TAX), 0.45)
    assert break_even_confidence(DEFAULT_TAX) < break_even_confidence(OFFICIAL_TAX)

def test_tax_cannot_touch_the_extremes():
    for tax in (0.0, 0.1, 0.5):
        close(omniscience_index(RIGHT_ALL, tax), 100.0)
        close(omniscience_index(WRONG_ALL, tax), -100.0)

def test_empty_bank_is_could_not_judge_not_zero():
    """0.0 is a real score on this scale -- it is what perfect abstention earns.
    A run that graded nothing must not be reportable as one (ARCH §18.3)."""
    assert omniscience_index({}, DEFAULT_TAX) is None
    assert accuracy({}) is None

def test_hallucination_rate_is_share_of_the_unknown():
    close(hallucination_rate(OPUS), 312 / (312 + 328))
    assert hallucination_rate(RIGHT_ALL) is None

def test_oracle_ceiling_is_110_times_accuracy_minus_10():
    """PLAN.md's route table is arithmetic, so it lives here rather than only in
    a markdown table where it can drift."""
    for acc in (0.182, 0.20, 0.25, 0.30, 0.36):
        c = round(acc * 600)
        hist = {"CORRECT": c, "INCORRECT": 250, "NOT_ATTEMPTED": 350 - c, "PARTIAL_ANSWER": 0}
        close(oracle_ceiling(hist, DEFAULT_TAX), 110 * (c / 600) - 10, eps=1e-6)

def test_oracle_ceiling_bounds_every_achievable_score():
    """The invariant that makes G1 a real gate: no abstention policy can beat it."""
    for c, i in ((360, 312), (120, 13), (137, 34), (0, 600), (600, 0)):
        hist = {"CORRECT": c, "INCORRECT": i, "NOT_ATTEMPTED": 600 - c - i, "PARTIAL_ANSWER": 0}
        assert oracle_ceiling(hist, DEFAULT_TAX) >= omniscience_index(hist, DEFAULT_TAX) - 1e-9

def test_reachability_floor_for_the_ten_target():
    """Below 18.2% accuracy, OI_taxed 10 is unreachable by calibration alone."""
    below = {"CORRECT": 108, "INCORRECT": 0, "NOT_ATTEMPTED": 492, "PARTIAL_ANSWER": 0}
    at    = {"CORRECT": 110, "INCORRECT": 0, "NOT_ATTEMPTED": 490, "PARTIAL_ANSWER": 0}
    assert oracle_ceiling(below, DEFAULT_TAX) < 10.0
    assert oracle_ceiling(at, DEFAULT_TAX) >= 10.0

if __name__ == "__main__":
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"  ok  {t.__name__}")
    print(f"\n{len(tests)} passed")
