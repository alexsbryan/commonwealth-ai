import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import score as S  # noqa: E402


def gold(files, deals, updates):
    return {
        "files": set(files),
        "people": [],
        "companies": [{"id": "f:c1", "name": "Customer Co", "domains": []}],
        "deals": deals,
        "stage_updates": updates,
        "commitments": [],
    }


def deal_atom(atom_id, section, counterparty="Customer Co", anchor=None, stage=None):
    first = {"chunk_id": section}
    if anchor is not None:
        first["passage_preview"] = anchor
    attributes = {"counterparty": counterparty}
    if stage is not None:
        attributes["stage"] = stage
    return {
        "atom_type": "Entity",
        "data": {
            "id": atom_id,
            "entity_type": "deal",
            "canonical_name": atom_id,
            "first_appearance": first,
            "attributes": attributes,
        },
    }


def stage_decision(atom_id, documents, values, outcome="decided"):
    """A derived_decisions.jsonl line for a deal's stage; documents are index source_doc_ids."""
    return {"type": "deal", "attribute": "stage", "atom": atom_id, "outcome": outcome,
            "values": values, "documents": documents}


def stage_claim(claim_id, subject, section, anchor, stage):
    return {
        "atom_type": "Claim",
        "data": {
            "id": claim_id,
            "claim_kind": "stage_update",
            "subject": subject,
            "anchor": anchor,
            "evidence": [{"chunk_id": section}],
            "attributes": {"stage": stage},
        },
    }


class WardScoreTests(unittest.TestCase):
    def setUp(self):
        S.DATES.clear()
        S.DOC_FILES.clear()

    def test_perfect_transaction_and_master_stages_are_reachable(self):
        S.DATES.update({
            "f/01": "2001-01-01",
            "f/02": "2001-01-02",
            "f/03": "2001-01-03",
        })
        g = gold(
            {"f/01", "f/02", "f/03"},
            [
                {"id": "f:tx", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01", "f/02"}},
                {"id": "f:master", "counterparty": "f:c1", "kind": "master_agreement", "files": {"f/03"}},
            ],
            [
                {"deal": "f:tx", "stage": "proposal", "file": "f/01"},
                {"deal": "f:tx", "stage": "won", "file": "f/02"},
                {"deal": "f:master", "stage": "negotiating", "file": "f/03"},
            ],
        )
        sec = {
            "tx-early": [("f/01", "sent a proposal")],
            "tx-late": [("f/02", "confirmed the trade")],
            "master": [("f/03", "master agreement draft exchanged")],
        }
        atoms = [
            deal_atom("atom-tx", "tx-early", stage="won"),
            deal_atom("atom-master", "master", stage="negotiating"),
            stage_claim("s1", "atom-tx", "tx-early", "sent a proposal", "proposal"),
            stage_claim("s2", "atom-tx", "tx-late", "confirmed the trade", "won"),
            stage_claim("s3", "atom-master", "master", "master agreement draft exchanged", "negotiating"),
        ]
        S.DOC_FILES.update({"doc-02": "f/02", "doc-03": "f/03"})
        decisions = [stage_decision("atom-tx", ["doc-02"], ["won"]),
                     stage_decision("atom-master", ["doc-03"], ["negotiating"])]

        ent, claims = S.load_atlas(atoms, sec, decisions)
        got = S.score(g, ent, claims, g["files"])

        self.assertEqual({"hit": 1, "n": 1, "recall": 1.0}, got["deals"])
        self.assertEqual({"hit": 1, "n": 1, "recall": 1.0}, got["master_agreements"])
        self.assertEqual({"hit": 3, "n": 3, "recall": 1.0}, got["stage"])
        self.assertEqual({"hit": 2, "n": 2, "recall": 1.0}, got["stage_served"])

    def test_unindexed_gold_file_stays_in_denominators(self):
        g = gold(
            {"f/01", "f/02"},
            [
                {"id": "f:present", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01"}},
                {"id": "f:unindexed", "counterparty": "f:c1", "kind": "transaction", "files": {"f/02"}},
            ],
            [
                {"deal": "f:present", "stage": "won", "file": "f/01"},
                {"deal": "f:unindexed", "stage": "proposal", "file": "f/02"},
            ],
        )
        sec = {"present": [("f/01", "confirmed")]}  # f/02 has no indexed section.
        atoms = [
            deal_atom("atom-present", "present", stage="won"),
            stage_claim("stage-present", "atom-present", "present", "confirmed", "won"),
        ]
        S.DOC_FILES["doc-01"] = "f/01"
        scopes = S.scoring_scopes(g["files"], {"f/01"})
        ent, claims = S.load_atlas(atoms, sec, [stage_decision("atom-present", ["doc-01"], ["won"])])
        got = S.score(g, ent, claims, scopes["all"])

        self.assertEqual({"f/01", "f/02"}, scopes["all"])
        self.assertEqual({"f/02"}, scopes["holdout"])
        self.assertEqual({"hit": 1, "n": 2, "recall": 0.5}, got["deals"])
        self.assertEqual({"hit": 1, "n": 2, "recall": 0.5}, got["stage"])
        self.assertEqual({"hit": 1, "n": 2, "recall": 0.5}, got["stage_served"])
        self.assertEqual(1, got["stage_served_missed"]["unmatched_deal"])

    def test_files_with_nothing_remain_in_gold_source_inventory(self):
        record = {
            "folder": "f",
            "files_read": ["1."],
            "files_with_nothing": ["2."],
            "people": [],
            "companies": [],
            "deals": [],
            "stage_updates": [],
            "commitments": [],
        }
        with tempfile.TemporaryDirectory() as directory:
            (pathlib.Path(directory) / "f.json").write_text(json.dumps(record))
            loaded = S.load_gold(pathlib.Path(directory))

        scopes = S.scoring_scopes(loaded["files"], set())
        self.assertEqual({"f/1.", "f/2."}, scopes["all"])
        self.assertEqual({"f/1.", "f/2."}, scopes["holdout"])

    def test_permuting_gold_and_atoms_preserves_counts_and_assignments(self):
        g = gold(
            {"f/01", "f/02"},
            [
                {"id": "f:alpha-flex", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01", "f/02"}},
                {"id": "f:beta-fixed", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01"}},
            ],
            [],
        )
        sec = {"one": [("f/01", "evidence one")], "two": [("f/02", "evidence two")]}
        atoms = [deal_atom("atom-1", "one"), deal_atom("atom-2", "two")]
        ent_a, claims_a = S.load_atlas(atoms, sec)
        ent_b, claims_b = S.load_atlas(list(reversed(atoms)), dict(reversed(list(sec.items()))))
        first = S.score(g, ent_a, claims_a, g["files"])
        permuted_gold = dict(g, deals=list(reversed(g["deals"])))
        second = S.score(permuted_gold, ent_b, claims_b, g["files"])

        expected = {"f:alpha-flex": "atom-2", "f:beta-fixed": "atom-1"}
        self.assertEqual(first["deals"], second["deals"])
        self.assertEqual(expected, first["deal_matches"]["transactions"])
        self.assertEqual(first["deal_matches"], second["deal_matches"])

    def test_maximum_matching_uses_an_augmenting_path(self):
        left = [{"id": "a-flex"}, {"id": "b-fixed"}]
        right = [{"id": "atom-1"}, {"id": "atom-2"}]
        edges = {"a-flex": {"atom-1", "atom-2"}, "b-fixed": {"atom-1"}}

        got = S.maximum_matching(left, right, lambda l, r: r["id"] in edges[l["id"]])

        self.assertEqual({"a-flex": "atom-2", "b-fixed": "atom-1"}, got)

    def test_wrong_party_does_not_match_or_license_stage(self):
        g = gold(
            {"f/01"},
            [{"id": "f:tx", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01"}}],
            [{"deal": "f:tx", "stage": "won", "file": "f/01"}],
        )
        atoms = [
            deal_atom("wrong-party", "only", "Other Corp", stage="won"),
            stage_claim("wrong-party-stage", "wrong-party", "only", "trade confirmed", "won"),
        ]
        S.DOC_FILES["doc-01"] = "f/01"
        decisions = [stage_decision("wrong-party", ["doc-01"], ["won"])]
        ent, claims = S.load_atlas(atoms, {"only": [("f/01", "trade confirmed")]}, decisions)

        got = S.score(g, ent, claims, g["files"])

        self.assertEqual(0, got["deals"]["hit"])
        self.assertEqual(0, got["stage"]["hit"])
        self.assertEqual(0, got["stage_served"]["hit"])
        self.assertEqual(1, got["stage_served_missed"]["unmatched_deal"])

    def test_bad_or_ambiguous_anchor_in_shared_section_is_refused(self):
        prefix = "r" * 80
        absent_anchor = "a phrase absent from both documents"
        prefix_only_anchor = prefix + " required ending"
        shared = [
            ("f/01", "the target message has no matching phrase; repeated evidence"),
            ("f/decoy", prefix + " incompatible ending; repeated evidence"),
        ]
        g = gold(
            {"f/01"},
            [{"id": "f:tx", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01"}}],
            [{"deal": "f:tx", "stage": "won", "file": "f/01"}],
        )
        atoms = [
            deal_atom("atom-tx", "target"),
            stage_claim("absent", "atom-tx", "shared", absent_anchor, "won"),
            stage_claim("prefix-only", "atom-tx", "shared", prefix_only_anchor, "won"),
            stage_claim("ambiguous", "atom-tx", "shared", "repeated evidence", "won"),
        ]
        ent, claims = S.load_atlas(atoms, {"target": [("f/01", "independent deal evidence")], "shared": shared})
        claim_files = {claim["id"]: claim["_files"] for claim in claims}

        got = S.score(g, ent, claims, g["files"])

        self.assertEqual(set(), claim_files["absent"])
        self.assertEqual(set(), claim_files["prefix-only"])
        self.assertEqual(set(), claim_files["ambiguous"])
        self.assertEqual(1, got["deals"]["hit"])
        self.assertEqual(0, got["stage"]["hit"])

    def test_transactions_keep_atom_priority_over_master_agreements(self):
        g = gold(
            {"f/shared"},
            [
                {"id": "f:tx", "counterparty": "f:c1", "kind": "transaction", "files": {"f/shared"}},
                {"id": "f:master", "counterparty": "f:c1", "kind": "master_agreement", "files": {"f/shared"}},
            ],
            [],
        )
        ent, claims = S.load_atlas([deal_atom("only-atom", "shared")], {"shared": [("f/shared", "both refer here")]})

        got = S.score(g, ent, claims, g["files"])

        self.assertEqual({"f:tx": "only-atom"}, got["deal_matches"]["transactions"])
        self.assertEqual({}, got["deal_matches"]["master_agreements"])
        self.assertEqual(1, got["deals"]["hit"])
        self.assertEqual(0, got["master_agreements"]["hit"])

    def test_served_stage_is_credited_only_with_its_provenance(self):
        S.DATES.update({"f/01": "2001-01-01", "f/02": "2001-01-02", "f/03": "2001-01-03"})
        S.DOC_FILES.update({"doc-01": "f/01", "doc-02": "f/02", "doc-03": "f/03"})
        g = gold(
            {"f/01", "f/02", "f/03"},
            [
                {"id": "f:tx", "counterparty": "f:c1", "kind": "transaction", "files": {"f/01", "f/02"}},
                {"id": "f:other", "counterparty": "f:c1", "kind": "transaction", "files": {"f/03"}},
            ],
            [
                {"deal": "f:tx", "stage": "proposal", "file": "f/01"},
                {"deal": "f:tx", "stage": "won", "file": "f/02"},
                {"deal": "f:other", "stage": "won", "file": "f/03"},
            ],
        )
        sec = {"tx": [("f/01", "sent a proposal")], "other": [("f/03", "other deal confirmed")]}

        def served(stage, decisions):
            atoms = [deal_atom("atom-tx", "tx", stage=stage)]
            ent, claims = S.load_atlas(atoms, sec, decisions)
            got = S.score(g, ent, claims, g["files"])
            return got["stage_served"]["hit"], {k: v for k, v in got["stage_served_missed"].items() if v}

        unmatched = {"unmatched_deal": 1}  # f:other has no atom in every case
        self.assertEqual((1, unmatched), served("won", [stage_decision("atom-tx", ["doc-02"], ["won"])]))
        # the right stage from a message gold places at another stage, or about another deal
        self.assertEqual((0, {**unmatched, "unplaced": 1}), served("won", [stage_decision("atom-tx", ["doc-01"], ["won"])]))
        self.assertEqual((0, {**unmatched, "unplaced": 1}), served("won", [stage_decision("atom-tx", ["doc-03"], ["won"])]))
        # one deciding document gold does not place; one the index cannot name is the instrument's could-not-judge
        self.assertEqual((0, {**unmatched, "unplaced": 1}),
                         served("won", [stage_decision("atom-tx", ["doc-02", "doc-01"], ["won"])]))
        self.assertEqual((0, {**unmatched, "provenance_unmapped": 1}),
                         served("won", [stage_decision("atom-tx", ["doc-02", "doc-unknown"], ["won"])]))
        # a decision disagreeing with the served value, or naming no document, does not back it
        self.assertEqual((0, {**unmatched, "unbacked": 1}),
                         served("won", [stage_decision("atom-tx", ["doc-02"], ["negotiating"])]))
        self.assertEqual((0, {**unmatched, "unbacked": 1}),
                         served("won", [stage_decision("atom-tx", [], ["won"])]))
        self.assertEqual((0, {**unmatched, "wrong_stage": 1}),
                         served("proposal", [stage_decision("atom-tx", ["doc-01"], ["proposal"])]))
        self.assertEqual((0, {**unmatched, "pending": 1}),
                         served(None, [stage_decision("atom-tx", [], ["won", "proposal"], outcome="pending")]))
        self.assertEqual((0, {**unmatched, "no_decision": 1}), served("won", []))
        self.assertEqual((0, {**unmatched, "no_decision": 1}), served(None, []))


if __name__ == "__main__":
    unittest.main()
