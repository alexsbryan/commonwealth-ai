import importlib.util
import json
import pathlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("support_score", pathlib.Path(__file__).with_name("score.py"))
S = importlib.util.module_from_spec(spec)
spec.loader.exec_module(S)


class ScoreContractTests(unittest.TestCase):
    def test_fold_keeps_noise_and_excludes_ambiguous_and_other_fold(self):
        docs = [{"id": d, "thread": t} for d, t in [("a", 1), ("missing", 1), ("noise", 1), ("ambiguous", 1), ("other", 2)]]
        pred = {d["id"]: "p" for d in docs if d["id"] != "missing"}
        scoped, counts = S.evaluation_scope(pred, {"a": "g", "missing": "g"}, {"ambiguous"}, {"noise"}, docs, "tune")
        self.assertEqual(scoped, {"a": "p", "noise": "p"})
        self.assertEqual(counts["no_case_documents_placed"], 1)
        self.assertEqual(counts["ambiguous_predictions_excluded"], 1)
        self.assertEqual(counts["predictions_outside_fold"], 1)

    def test_unknown_predictions_and_unlabelled_in_fold_refuse(self):
        docs = [{"id": "a", "thread": 1}, {"id": "unlabelled", "thread": 1}]
        for bad in ["fabricated", "unlabelled"]:
            with self.assertRaises(ValueError):
                S.evaluation_scope({bad: "p"}, {"a": "g"}, set(), set(), docs, "tune")

    def test_source_id_duplicates_and_missing_gold_source_refuse(self):
        with self.assertRaises(ValueError):
            S.evaluation_scope({}, {"a": "g"}, set(), set(), [{"id": "a", "thread": 1}] * 2, "tune")
        with self.assertRaises(ValueError):
            S.evaluation_scope({}, {"absent": "g"}, set(), set(), [{"id": "a", "thread": 1}], "tune")

    def test_state_free_member_requires_existing_case_record(self):
        atoms = [
            {"atom_type": "Entity", "data": {"id": "case", "entity_type": "case"}},
            {"atom_type": "Claim", "data": {"claim_kind": "membership", "subject": "case", "attributes": {"document_id": "member"}}},
            {"atom_type": "Claim", "data": {"claim_kind": "membership", "subject": "ghost", "attributes": {"document_id": "dangling"}}},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp, "atoms.json")
            path.write_text(json.dumps({"atoms": atoms}))
            pred, counts = S.atlas_pred(str(path), claim_kind="membership")
        self.assertEqual(pred, {"member": "case"})
        self.assertEqual(counts["claims_without_case_record"], 1)

    def test_shared_cli_penalizes_partial_recovery_and_noise(self):
        gold = {"a": "g1", "b": "g1", "c": "g2", "d": "g2"}
        result = S.er_score({"a": "p1", "c": "p2"}, gold)
        self.assertEqual(result["b_cubed"]["f1"], 1.0)
        self.assertAlmostEqual(result["recovery_b_cubed"]["f1"], 0.4)
        result = S.er_score({"a": "p1", "c": "p2", "noise": "p1"}, gold)
        self.assertLess(result["recovery_b_cubed"]["precision"], 1.0)

    def test_multiple_record_memberships_are_counted_without_a_vote(self):
        atoms = [{"atom_type": "Entity", "data": {"id": c, "entity_type": "case"}} for c in ["c1", "c2"]]
        atoms += [{"atom_type": "Claim", "data": {"claim_kind": "membership", "subject": c,
                  "attributes": {"document_id": "ambiguous"}}} for c in ["c1", "c1", "c2"]]
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp, "atoms.json")
            path.write_text(json.dumps({"atoms": atoms}))
            pred, counts = S.atlas_pred(str(path), claim_kind="membership")
        self.assertEqual(pred, {})
        self.assertEqual(counts["documents_in_several_records"], 1)


if __name__ == "__main__":
    unittest.main()
