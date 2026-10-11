"""agreement.py's comparison: identical partitions agree and differ nowhere; each kind of difference is named."""
import pathlib, sys, unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
sys.dont_write_bytecode = True
import agreement as A  # noqa: E402


class Compare(unittest.TestCase):
    GOLD = {"a": "g1", "b": "g1", "c": "g2", "d": "g2", "e": "g3"}

    def test_identical_partitions_agree_and_differ_nowhere(self):
        ours = {"a": "r1", "b": "r1", "c": "r2", "d": "r2", "e": None}
        s, rows = A.compare("x", set(self.GOLD), self.GOLD, ours, dict(ours))
        self.assertEqual((s["b_cubed"]["f1"], s["differences"], s["both_placed"]), (1.0, 0, 4))

    def test_each_difference_is_named_with_the_side_gold_agrees_with(self):
        ours = {"a": "r1", "b": "r1", "c": "r2", "d": "r3", "e": "r4"}
        blind = {"a": "s1", "b": "s2", "c": "s3", "d": "s3", "e": None}
        s, rows = A.compare("x", set(self.GOLD), self.GOLD, ours, blind)
        got = {x["unit"]: (x["kind"], x["closer"]) for x in rows}
        self.assertEqual(got, {"a": ("blind_splits", "ours"), "b": ("blind_splits", "ours"),
                               "c": ("blind_joins", "blind"), "d": ("blind_joins", "blind"), "e": ("blind_unplaced", None)})
        self.assertLess(s["b_cubed"]["f1"], 1.0)

    def test_a_row_no_rule_classes_is_unclassed_never_defaulted(self):
        rows = [{"system": "x", "kind": "blind_joins", "closer": "ours"}, {"system": "x", "kind": "blind_splits"}]
        n = A.classify(rows, [{"system": "x", "kind": "blind_joins", "class": "method gap", "why": "w"}])
        self.assertEqual(n, {("x", "method gap"): 1, ("x", "unclassed"): 1})


if __name__ == "__main__":
    unittest.main()
