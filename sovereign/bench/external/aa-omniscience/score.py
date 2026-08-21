# SPDX-License-Identifier: AGPL-3.0-or-later
"""AA-Omniscience scoring, with our abstention tax.

ONE formula, one decider (ARCH §10.6). The official Artificial Analysis
metric is the `tax=0.0` special case of ours, not a separate code path:

    OI(tax) = 100 * (c - i - tax*a) / (c + p + i + a)

WHY THE TAX EXISTS. Official OI gives abstention weight 0, so a harness that
declines every question scores exactly 0 -- which at the Nov-2025 snapshot
would have placed 4th overall, above every model but three. That is a
degenerate strategy the official metric does not price, and it is precisely
the strategy a restraint-shaped harness drifts into. A mild tax removes the
free ride without inverting the incentive that makes the benchmark
interesting: at tax=0.1 a model should still answer only when it is better
than 45% confident (vs 50% officially), so abstention remains strongly
preferred over guessing.

`oi_official` is ALWAYS reported alongside `oi_taxed`. The tax is our
deviation and it is named at every surface that prints a number (ARCH §18.3
-- never silently substitute).
"""

GRADES = ("CORRECT", "PARTIAL_ANSWER", "INCORRECT", "NOT_ATTEMPTED")

OFFICIAL_TAX = 0.0
DEFAULT_TAX = 0.1


def omniscience_index(counts, tax=OFFICIAL_TAX):
    """OI over a grade histogram. Returns None on an empty bank.

    None, not 0.0: a run that graded nothing is a could-not-judge, and 0.0
    is a real score on this scale (it is what perfect abstention earns).
    """
    n = sum(counts.get(g, 0) for g in GRADES)
    if n == 0:
        return None
    c = counts.get("CORRECT", 0)
    i = counts.get("INCORRECT", 0)
    a = counts.get("NOT_ATTEMPTED", 0)
    return 100.0 * (c - i - tax * a) / n


def accuracy(counts):
    """Share of the bank answered correctly. The competence floor that stops
    the index from being read on its own."""
    n = sum(counts.get(g, 0) for g in GRADES)
    return None if n == 0 else counts.get("CORRECT", 0) / n


def hallucination_rate(counts):
    """AA's definition: incorrect guesses as a share of the questions the
    model did not know -- i / (i + a). None when it knew everything."""
    i = counts.get("INCORRECT", 0)
    a = counts.get("NOT_ATTEMPTED", 0)
    return None if (i + a) == 0 else i / (i + a)


def break_even_confidence(tax=OFFICIAL_TAX):
    """Confidence above which answering beats abstaining under OI(tax).

    E[answer] = 2p - 1;  E[abstain] = -tax;  answer iff p > (1 - tax)/2.
    This is the knob's real meaning -- a tax is a shift in the confidence
    threshold a rational harness should gate at, nothing more.
    """
    return (1.0 - tax) / 2.0


def summarize(counts, tax=DEFAULT_TAX):
    """Every number this lane is allowed to report, in one dict."""
    return {
        "n_graded": sum(counts.get(g, 0) for g in GRADES),
        "counts": {g: counts.get(g, 0) for g in GRADES},
        "accuracy": accuracy(counts),
        "hallucination_rate": hallucination_rate(counts),
        "oi_official": omniscience_index(counts, OFFICIAL_TAX),
        "oi_taxed": omniscience_index(counts, tax),
        "abstention_tax": tax,
        "break_even_confidence": break_even_confidence(tax),
    }
