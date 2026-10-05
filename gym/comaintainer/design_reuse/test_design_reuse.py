#!/usr/bin/env python3
"""Tests for the design-reuse lane: bank integrity, leakage, scoring, rescore.

    python3 gym/comaintainer/design_reuse/test_design_reuse.py

No model calls. The bank's real git history is read read-only; the rescore
test uses a synthetic run directory and must reproduce metrics with only
the persisted JSON.
"""
from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import common as C  # noqa: E402
import replay as R  # noqa: E402

GOOD = {
    "disposition": "USE",
    "owner": "sovereign-cli-dev::converge_cmd",
    "seam": "converge shape",
    "delta": "Adopt svrn code converge shape (sovereign/crates/sovereign-cli-dev/src/converge_cmd.rs); "
             "it already measures field-shape identity independent of type names. No new code.",
    "new_components": [],
    "limits": "Discovery only; ownership still needs adjudication.",
    "evidence": ["converge-cli-shape"],
}

BAD = {
    "disposition": "ADD",
    "owner": "new renamed-fork detector",
    "seam": "hand census",
    "delta": "Build a new detector and run a hand census over the workspace.",
    "new_components": ["new detector"],
    "limits": "None.",
    "evidence": [],
}


class BankTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = C.load_cases()
        cls.by_id = {c["id"]: c for c in cls.cases}

    def test_bank_verifies_clean_with_both_splits(self):
        self.assertGreaterEqual(len(self.cases), 8)
        problems = []
        for case in self.cases:
            problems += C.verify_case(case)
        self.assertEqual(problems, [])
        splits = C.assign_splits(self.cases)
        self.assertTrue(any(s == "holdout" for s in splits.values()))
        self.assertTrue(any(s == "dev" for s in splits.values()))
        self.assertEqual(splits, C.assign_splits(self.cases))  # deterministic

    def test_leak_detector_catches_planted_correction(self):
        case = copy.deepcopy(self.by_id["reuse-tool-discovery-converge-shape"])
        case["request"]["constraints"].append(case["oracle"]["correction_quote"])
        bad = C.verify_case(case)
        self.assertTrue(any("leaks into request" in b for b in bad), bad)

    def test_leak_detector_catches_planted_outcome_sha(self):
        case = copy.deepcopy(self.by_id["reuse-tool-discovery-converge-shape"])
        case["request"]["requirement"] += " (see " + case["provenance"]["outcome_sha"] + ")"
        bad = C.verify_case(case)
        self.assertTrue(any("outcome sha appears" in b for b in bad), bad)

    def test_scorer_accepts_grounded_proposal_and_flags_bait(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        good = C.score_proposal(case, GOOD, "C")
        self.assertTrue(good["disposition_match"])
        self.assertTrue(good["home_match"])
        self.assertTrue(good["path_match"])
        self.assertEqual(good["new_components"], 0)
        self.assertEqual(good["bait_hits"], [])
        self.assertTrue(good["evidence_grounded"])
        bad = C.score_proposal(case, BAD, "C")
        self.assertFalse(bad["disposition_match"])
        self.assertFalse(bad["home_match"])
        self.assertGreaterEqual(len(bad["bait_hits"]), 2)
        self.assertFalse(bad["evidence_grounded"])
        # Evidence grounding is only required under the C protocol.
        self.assertIsNone(C.score_proposal(case, BAD, "A")["evidence_grounded"])

    def test_prompts_hide_oracle_and_dossier_separates_conditions(self):
        for case in self.cases:
            prompt_a = C.norm(C.build_prompt(case, "A"))
            self.assertNotIn(case["provenance"]["outcome_sha"], prompt_a)
            self.assertNotIn(C.norm(case["oracle"]["correction_quote"]), prompt_a)
            first = case["dossier"]["candidates"][0]
            self.assertNotIn(C.norm(first["excerpt"]), prompt_a,
                             f"{case['id']}: dossier leaked into condition A")
            self.assertIn(C.norm(first["excerpt"]), C.norm(C.build_prompt(case, "B")))

    def test_schema_c_requires_grounding_fields(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        c = C.schema_for(case, "C")
        self.assertIn("evidence", c["required"])
        self.assertIn("limits", c["required"])
        self.assertEqual(c["properties"]["evidence"]["minItems"], 1)
        enum = c["properties"]["evidence"]["items"]["enum"]
        self.assertIn("converge-cli-shape", enum)          # grounding is enumerated
        self.assertIn("none-of-these", enum)               # ADD escape stays expressible
        b = C.schema_for(case, "B")
        self.assertNotIn("evidence", b["required"])
        self.assertNotIn("limits", b["required"])


class RescoreTests(unittest.TestCase):
    def test_rescore_reproduces_metrics_without_calls(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            (run / "meta.json").write_text(json.dumps({"stamp": "test"}))
            rows = [
                {"case": "reuse-tool-discovery-converge-shape", "condition": "A",
                 "verdict": "parsed", "reason": None, "parsed": GOOD,
                 "raw": "{}", "model": "test", "metrics": None},
                {"case": "reuse-tool-discovery-converge-shape", "condition": "A",
                 "verdict": "could-not-judge", "reason": "http503",
                 "parsed": None, "raw": None, "model": None, "metrics": None},
            ]
            (run / "calls.jsonl").write_text(
                "".join(json.dumps(r) + "\n" for r in rows))
            summary = R.rescore_dir(run)
            agg = summary["aggregate"]["A"]
            self.assertEqual(agg["n"], 2)
            self.assertEqual(agg["could_not_judge"], 1)
            self.assertEqual(agg["home_match"], 1)
            self.assertEqual(agg["path_match"], 1)
            self.assertTrue((run / "rescore.json").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
