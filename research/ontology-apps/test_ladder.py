"""The stage ladder's tests: the matcher, the stage decider, GVC's line-to-mention alignment on its fixture, the
Judgement verdict, and the moved instrument against the 2026-10-09 baseline row.

    python3 test_ladder.py
"""
import contextlib, io, json, pathlib, sys, tempfile, unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402
import ladder_gvc as G  # noqa: E402

FIX = HERE / "fixtures/gvc-e2e-mini"
A, B, C = "b27405f9afd055f54cdab7c68ccff13e", "5a0c56265670498621b7cd2f71e53902", "104db82506283933234d28c49929a9cc"


class Matcher(unittest.TestCase):
    def test_assign_maximises_the_total_where_greedy_would_not(self):
        w = {("a", "x"): 3, ("a", "y"): 2, ("b", "x"): 2}  # greedy takes a-x (3); the optimum is a-y + b-x (4)
        self.assertEqual(L.assign(w), {"a": "y", "b": "x"})
        self.assertEqual(L.assign({("a", "x"): 1, ("b", "x"): 5}), {"b": "x"})
        self.assertEqual(L.assign({}), {})


class Decider(unittest.TestCase):
    def test_first_lost_stage_in_read_place_fold_order(self):
        self.assertEqual(L.Facts(False, False, False).stage(), L.Stage.READ)
        self.assertEqual(L.Facts(True, False, True).stage(), L.Stage.PLACE)
        self.assertEqual(L.Facts(True, True, False).stage(), L.Stage.FOLD)
        self.assertEqual(L.Facts(True, True, True).stage(), "hit")

    def test_an_absent_fact_is_could_not_judge_never_a_loss(self):
        self.assertEqual(L.Facts(True, True, None).stage(), ("could_not_judge", L.Stage.FOLD))
        self.assertEqual(L.Facts(False, None, None).stage(), L.Stage.READ)  # a loss before it decides
        s = L.summarize([L.Facts(True, True, None), L.Facts(True, True, True)], None)
        self.assertEqual((s["hit"], s["stages"]["fold"]["lost"], s["stages"]["fold"]["could_not_judge"]), (1, 0, 1))

    def test_an_unregistered_phase_is_reported_by_name_not_dropped(self):
        c = L.calls_by_question({"documents_read": 4, "calls_by_phase": {"document_passes_locate": 8, "new_phase": 2}})
        self.assertEqual(c["read"], {"Locate": {"calls": 8, "per_document": 2.0}})
        self.assertEqual(c["unstaged"], {"new_phase": {"calls": 2, "per_document": 0.5}})


class Alignment(unittest.TestCase):
    BODY = "Title line\nA man was shot .\nPolice say he died .\nPolice say more ."

    def test_find_exact_folded_ambiguous_absent(self):
        self.assertEqual(G.find("A man was shot .", self.BODY), (11, 27))
        start, end = G.find("A man  was\nshot .", self.BODY)
        self.assertEqual(self.BODY[start:end], "A man was shot .")
        self.assertEqual(G.find("Police say", self.BODY), "ambiguous")
        self.assertEqual(G.find("not here", self.BODY), "absent")
        self.assertEqual(G.find("", self.BODY), "absent")

    def test_cited_lines_are_whole_lines_the_span_touches(self):
        lines = G.line_spans(self.BODY)
        self.assertEqual(G.cited((15, 18), lines), lines[1])  # "was" inside line 1 -> all of line 1
        self.assertEqual(G.cited((11, 34), lines), (lines[1][0], lines[2][1]))  # crosses into line 2

    def test_a_mention_whose_offsets_do_not_read_back_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = pathlib.Path(tmp)
            (t / "raw").mkdir()
            (t / "gold").mkdir()
            (t / "raw/documents.jsonl").write_text(json.dumps({"id": "d", "body": "a man was shot"}) + "\n")
            (t / "gold/mentions.jsonl").write_text(json.dumps(
                {"doc_id": "d", "mention_id": "d/m", "start": 0, "end": 3, "text": "shot", "type": "hitting"}) + "\n")
            (t / "gold.json").write_text(json.dumps({"d/m": "c"}))
            with self.assertRaises(SystemExit):
                G.load_gold(t / "gold.json", t)


class GvcFixture(unittest.TestCase):
    """fixtures/gvc-e2e-mini (build.py): every mention's stage, stated by hand from the fixture's claims."""

    @classmethod
    def setUpClass(cls):
        cls.r = G.measure(FIX / "run", gold=FIX / "gold.json", corpus=FIX / "corpus")

    def test_every_mention_reads_the_stage_its_claims_give_it(self):
        hit = {f"{A}/s3t3-5", f"{B}/s5t12-13", f"{B}/s6t12-15"}  # on rec-death, which the death chain matches
        fold = {f"{A}/s1t6-7", f"{B}/s0t5-6"}  # firing chain on rec-fire, whose kind is "the incident as a whole"
        place = {f"{A}/s1t3-4", f"{B}/s0t6-7",  # death mentions on rec-fire, not the death chain's record
                 f"{A}/s4t3-4", f"{B}/s5t13-14"}  # their chains matched no record
        unjudged = {f"{B}/s4t11-12"}  # cited by line, on rec-odd, whose value the fold map does not name
        got = {x["mention"]: x["ladder_stage"] for x in self.r["items"]}
        self.assertEqual(len(got), 26)
        for mid, stage in got.items():
            want = ("hit" if mid in hit else "fold" if mid in fold else "place" if mid in place
                    else "could_not_judge:fold" if mid in unjudged else "read")
            self.assertEqual(stage, want, mid)
        self.assertEqual({x["mention"] for x in self.r["items"] if x["rung"] == "document_not_read"},
                         {f"{C}/s0t0-1", f"{C}/s2t7-8"})

    def test_every_claim_is_aligned_or_counted(self):
        self.assertEqual(self.r["alignment"], {"claims": 10, "claims_aligned": 6, "claims_anchor_ambiguous": 1,
                                               "claims_anchor_absent": 1, "claims_document_not_in_gold": 1,
                                               "claims_not_about_a_record": 1})
        self.assertEqual(self.r["detail"]["statements_covering_several_chains"], 3)

    def test_ladder_identity_and_calls(self):
        lad = self.r["ladder"]
        self.assertEqual((lad["n"], lad["hit"]), (26, 3))
        self.assertEqual({s: (v["lost"], v["could_not_judge"]) for s, v in lad["stages"].items()},
                         {"read": (16, 0), "place": (4, 0), "fold": (2, 1)})
        self.assertEqual(lad["stages"]["place"]["calls"]["RESOLVE forced choice"], {"calls": 4, "per_document": 2.0})
        self.assertEqual(lad["unstaged_calls"], {"phase_nobody_registered": {"calls": 2, "per_document": 1.0}})
        # B3 by hand over the 10 mentions covered on one record: precision 5.3/10, recall 7.6/10.
        self.assertEqual((self.r["identity"]["b_cubed"], self.r["identity"]["scored"]), (0.624, 10))

    def test_a_run_whose_records_carry_no_fold_field_cannot_judge_fold(self):
        with tempfile.TemporaryDirectory() as tmp:
            spec = json.loads((HERE / "cdcr/fold_values.json").read_text())
            spec["field"] = "no_such_field"
            p = pathlib.Path(tmp, "spec.json")
            p.write_text(json.dumps(spec))
            r = G.measure(FIX / "run", gold=FIX / "gold.json", corpus=FIX / "corpus", spec=p)
        self.assertEqual(r["ladder"]["stages"]["fold"], dict(r["ladder"]["stages"]["fold"], lost=0, could_not_judge=6))
        self.assertTrue(r["detail"]["fold"].startswith("could_not_judge"))


class Verdict(unittest.TestCase):
    def judged(self, s):
        return {"system": s, "status": "judged", "ladder": {"n": 1}}

    def test_four_verdicts(self):
        self.assertEqual(L.verdict([self.judged("a"), self.judged("b")])[0], "passed")
        self.assertEqual(L.verdict([self.judged("a"), L.never_ran("b", "no run")])[0], "never-ran")
        self.assertEqual(L.verdict([self.judged("a"), {"system": "b", "status": "failed", "reason": "x"}])[0], "failed")
        self.assertEqual(L.verdict([{"system": "b", "status": "could-not-judge", "reason": "x"},
                                    L.never_ran("c", "no run")])[0], "could-not-judge")

    def test_a_missing_run_reads_never_ran_not_zero(self):
        self.assertEqual(L.run_system("gvc", None)["status"], "never-ran")
        self.assertEqual(L.run_system("ward", pathlib.Path("/nonexistent/run"))["status"], "never-ran")


class Reproduction(unittest.TestCase):
    def test_the_moved_instrument_reproduces_the_baseline_row(self):
        if not (L.BASELINE / "uv").exists() or not (L.BASELINE / "ward").exists():
            self.skipTest(f"could not judge: the baseline runs are not on this host ({L.BASELINE})")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            ok = L.reproduce()
        self.assertTrue(ok, out.getvalue())


if __name__ == "__main__":
    unittest.main()
