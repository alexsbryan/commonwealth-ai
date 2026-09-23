"""Regression checks for the actual committed study-1 evidence, not titles."""

import unittest

from w1_determinism import compare, fingerprint, load
from w2_displacement import QUOTES


class AttributionTests(unittest.TestCase):
    def test_equal_titles_do_not_mean_equal_passages(self):
        first = load("full", 1)["list-igch0077-mints"]
        second = load("full", 2)["list-igch0077-mints"]
        self.assertEqual(
            [c["title"] for c in first["retrieved"]],
            [c["title"] for c in second["retrieved"]],
        )
        self.assertNotEqual(fingerprint(first), fingerprint(second))

    def test_answer_changes_always_have_recorded_evidence_changes_on_full(self):
        classes = compare("full", verbose=False)
        self.assertEqual(sum(n for (answer, evidence, _), n in classes.items() if not answer and evidence), 0)
        self.assertEqual(sum(n for (_, evidence, _), n in classes.items() if not evidence), 41)

    def test_k0_quotes_survive_bare_but_not_grounded_arms(self):
        for question, quote in list(QUOTES.items())[:4]:
            for arm in ("bare", "grounding-only", "full"):
                for run in (1, 2, 3):
                    row = load(arm, run)[question]
                    admitted = [c["prompt_text"] for c in row["retrieved"] if c.get("in_prompt")]
                    self.assertEqual(
                        any(quote.casefold() in text.casefold() for text in admitted),
                        arm == "bare",
                        (question, arm, run),
                    )


if __name__ == "__main__":
    unittest.main()
