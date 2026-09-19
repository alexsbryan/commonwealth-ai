#!/usr/bin/env python3
import importlib.util
import pathlib
import sys
import tempfile
import unittest


PATH = pathlib.Path(__file__).resolve().parents[1] / "test-behavior-census.py"
SPEC = importlib.util.spec_from_file_location("behavior_census_impl", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules["behavior_census_impl"] = MODULE
SPEC.loader.exec_module(MODULE)


class BehaviorCensusTests(unittest.TestCase):
    def witness(self, path, name, behavior, boundary=()):
        return MODULE.TestWitness(
            f"{path}/{name}().", name, path, 0, 4, behavior,
            MODULE.normalize_tokens(behavior), tuple(boundary),
        )

    def test_source_behavior_uses_scip_range_and_preceding_doc(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            source = root / "tests.rs"
            source.write_text(
                "/// Refuses a replayed proof.\n"
                "#[test]\n"
                "fn replay_is_refused() {\n"
                "    assert!(!verify(), \"replayed proof is refused\");\n"
                "}\n"
            )
            behavior, tokens = MODULE.source_behavior(
                root, ("q", "replay_is_refused", "tests.rs", 2, 4)
            )
            self.assertIn("Refuses a replayed proof", behavior)
            self.assertIn("replayed proof is refused", behavior)
            self.assertIn("replayed", tokens)

    def test_shared_boundary_recommends_contract_and_layered_proof(self):
        boundary = (("prod::merge().", "merge"),)
        left = self.witness("a/tests.rs", "older_invite_rejected", "older invite never wins", boundary)
        right = self.witness("b/tests.rs", "older_dial_rejected", "older dial info never wins", boundary)
        found = MODULE.candidates([left, right], 0.1)
        self.assertEqual(len(found), 1)
        self.assertIn("shared contract", found[0].recommendation)
        self.assertEqual(found[0].shared_boundaries[0][1], "merge")

    def test_related_behavior_without_shared_boundary_stays_layered(self):
        left = self.witness("a/tests.rs", "seal_refused", "a refused seal retires nothing")
        right = self.witness("b/tests.rs", "forged_seal_no_delete", "a refused seal deletes nothing")
        found = MODULE.candidates([left, right], 0.1)
        self.assertEqual(len(found), 1)
        self.assertIn("retain layered", found[0].recommendation)


if __name__ == "__main__":
    unittest.main()
