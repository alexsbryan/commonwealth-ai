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
import loop as D  # noqa: E402
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


class LoopTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = C.load_cases()
        cls.by_id = {c["id"]: c for c in cls.cases}

    @staticmethod
    def obs_for(case, **overrides):
        out = []
        for cand in case["dossier"]["candidates"]:
            out.append({"candidate": cand["id"], "usable": cand["facts"]["usable"],
                        "serves": cand["facts"]["serves"]})
        for cid, fields in overrides.items():
            for obs in out:
                if obs["candidate"] == cid:
                    obs.update(fields)
        return out

    @staticmethod
    def action(case, observations, disposition, candidate):
        return json.dumps({
            "observations": observations, "disposition": disposition,
            "candidate": candidate, "delta": "Reuse the recorded surface. " * 3,
            "new_components": [], "limits": "No known limitation for this task.",
            "evidence": [case["dossier"]["candidates"][0]["id"]],
        })

    @staticmethod
    def scripted(replies):
        pending = list(replies)
        prompts = []

        def ask(prompt, schema):
            prompts.append(prompt)
            return pending.pop(0), "scripted"
        ask.prompts = prompts
        return ask

    def test_loop_grading_and_consistency(self):
        converge = self.by_id["reuse-tool-discovery-converge-shape"]
        self.assertEqual(D.grade_observations(converge, self.obs_for(converge)),
                         ([], set(), []))
        wrong, _, _ = D.grade_observations(
            converge, self.obs_for(converge, **{"converge-cli-shape": {"serves": False}}))
        self.assertEqual(wrong, ["converge-cli-shape.serves"])
        # Reading: a serving surface admits only USE; every build is refused.
        self.assertIsNone(D.consistency(converge, {"disposition": "USE", "candidate": "converge-cli-shape"}))
        self.assertIsNotNone(D.consistency(converge, {"disposition": "EXTEND", "candidate": "converge-cli-shape"}))
        self.assertIsNotNone(D.consistency(converge, {"disposition": "EXTEND", "candidate": "behaviour-detector-scope"}))
        self.assertIsNotNone(D.consistency(converge, {"disposition": "ADD", "candidate": "none"}))
        self.assertIsNotNone(D.consistency(converge, {"disposition": "USE", "candidate": "behaviour-detector-scope"}))
        core = self.by_id["contrast-core-read-port"]
        self.assertIsNone(D.consistency(core, {"disposition": "EXTEND", "candidate": "index-source"}))
        self.assertIsNotNone(D.consistency(core, {"disposition": "USE", "candidate": "index-source"}))
        self.assertIsNotNone(D.consistency(core, {"disposition": "EXTEND", "candidate": "corpus-engine-concrete"}))
        self.assertIsNone(D.consistency(core, {"disposition": "ADD", "candidate": "none"}))

    def test_loop_driver_refuses_false_observation_then_accepts(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        bad = self.action(case, self.obs_for(case, **{"converge-cli-shape": {"serves": False}}),
                          "USE", "converge-cli-shape")
        good = self.action(case, self.obs_for(case), "USE", "converge-cli-shape")
        row = D.run_case_loop(case, self.scripted([bad, good]))
        self.assertTrue(row["finished"])
        self.assertEqual(row["refusals"], 1)
        self.assertEqual(row["wrong_observations"], 1)
        self.assertEqual(row["attempts"], 2)
        self.assertEqual(row["first_attempt_wrong"], 1)
        self.assertIn("converge-cli-shape.serves", row["attempt_log"][0]["refused"])
        m = row["metrics"]
        self.assertTrue(m["disposition_match"])
        self.assertTrue(m["home_match"])
        self.assertTrue(m["evidence_grounded"])

    def test_loop_driver_refuses_inconsistent_disposition(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        add = self.action(case, self.obs_for(case), "ADD", "none")
        use = self.action(case, self.obs_for(case), "USE", "converge-cli-shape")
        row = D.run_case_loop(case, self.scripted([add, use]))
        self.assertTrue(row["finished"])
        self.assertEqual(row["refusals"], 1)
        self.assertEqual(row["wrong_observations"], 0)
        self.assertTrue(row["metrics"]["disposition_match"])

    def test_loop_missing_observation_is_refused_with_message(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        short = dict(json.loads(self.action(case, self.obs_for(case), "USE", "converge-cli-shape")))
        short["observations"] = short["observations"][:-1]
        good = self.action(case, self.obs_for(case), "USE", "converge-cli-shape")
        ask = self.scripted([json.dumps(short), good])
        row = D.run_case_loop(case, ask)
        self.assertTrue(row["finished"])
        self.assertEqual(row["refusals"], 1)
        self.assertIn("observed exactly once", ask.prompts[1])

    def test_loop_cap_is_not_a_pass(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        wrong = self.action(case, self.obs_for(case, **{"converge-cli-shape": {"serves": False}}),
                            "USE", "converge-cli-shape")
        row = D.run_case_loop(case, self.scripted([wrong]), max_refusals=0)
        self.assertFalse(row["finished"])
        self.assertEqual(row["verdict"], "could-not-judge")
        self.assertIn("refusal cap", row["reason"])

    def test_loop_schema_enumerates_observations_and_evidence(self):
        case = self.by_id["contrast-core-read-port"]
        s = D.schema_d(case)
        ids = [c["id"] for c in case["dossier"]["candidates"]]
        self.assertEqual(s["properties"]["observations"]["minItems"], len(ids))
        self.assertEqual(s["properties"]["observations"]["maxItems"], len(ids))
        self.assertEqual(
            s["properties"]["observations"]["items"]["properties"]["candidate"]["enum"], ids)
        self.assertIn("none-of-these", s["properties"]["evidence"]["items"]["enum"])
        self.assertEqual(s["properties"]["candidate"]["enum"][-1], "none")

    def test_loop_no_restate_treats_record_as_state(self):
        case = self.by_id["reuse-tool-discovery-converge-shape"]
        s = D.schema_d(case, restate=False)
        self.assertNotIn("observations", s["properties"])
        self.assertNotIn("observations", s["required"])
        good = json.dumps({
            "disposition": "USE", "candidate": "converge-cli-shape",
            "delta": "Adopt the existing detector; no new code. " * 2,
            "new_components": [], "limits": "Discovery only; ownership is adjudicated.",
            "evidence": ["converge-cli-shape"],
        })
        ask = self.scripted([good])
        row = D.run_case_loop(case, ask, restate=False)
        self.assertTrue(row["finished"])
        self.assertEqual(row["refusals"], 0)
        self.assertTrue(row["metrics"]["disposition_match"])
        self.assertTrue(row["metrics"]["home_match"])
        bad = json.dumps({
            "disposition": "ADD", "candidate": "none",
            "delta": "Build a new detector from scratch. " * 2,
            "new_components": ["detector"], "limits": "None known.",
            "evidence": ["converge-cli-shape"],
        })
        ask = self.scripted([bad, good])
        row = D.run_case_loop(case, ask, restate=False)
        self.assertTrue(row["finished"])
        self.assertEqual(row["refusals"], 1)


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
