"""The stage ladder's tests: the matcher, the stage decider, GVC's line-to-mention alignment on its fixture, the
Judgement verdict, and the moved instrument against the 2026-10-09 baseline row.

    python3 test_ladder.py
"""
import contextlib, io, json, os, pathlib, subprocess, sys, tempfile, unittest

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


REPLAY_MARK = "2026-10-09T00:00:01.000Z === step replay_extract start: svrn-ingest enrich extract cdcr-gvc --asker replay"


def scaffold_leg(root, debug_recorded=None, job_recorded=None):
    """A run shaped like a runs/scaffold-3sys leg, built from fixtures/gvc-e2e-mini: the recorded atlas in
    recorded/ with its records as Event atoms (an event type's records since 4314b062f), a different replay atlas in
    the atlas dir, and logs whose replay half (after run_step.py's `=== step replay_extract start`) repeats the
    reader's lines with no model call."""
    atoms = json.loads((FIX / "run/data/indexes/cdcr-gvc/atlas/atoms.json").read_text())["atoms"]
    for a in atoms:
        if a["atom_type"] == "Entity":
            d = a["data"]
            a["atom_type"] = "Event"
            a["data"] = {"id": d["id"], "description": "x", "event_type": d.pop("entity_type"), "participants": [],
                         "evidence": [], "attributes": d["attributes"]}
    (root / "recorded").mkdir(parents=True)
    (root / "recorded/atoms.json").write_text(json.dumps({"atoms": atoms}))
    atlas = root / "data/indexes/cdcr-gvc/atlas"
    atlas.mkdir(parents=True)
    (atlas / "atoms.json").write_text(json.dumps({"atoms": [a for a in atoms if a["atom_type"] != "Claim"]}))
    dbg = (FIX / "run/job.debug.log").read_text() if debug_recorded is None else debug_recorded
    job = (FIX / "run/job.log").read_text() if job_recorded is None else job_recorded
    located = "".join(x + "\n" for x in (FIX / "run/job.debug.log").read_text().splitlines() if "located" in x)
    (root / "job.debug.log").write_text(dbg + REPLAY_MARK + "\n" + located)
    (root / "job.log").write_text(job + REPLAY_MARK + "\n" + (FIX / "run/job.log").read_text())
    return root


class ScaffoldLeg(unittest.TestCase):
    """A scaffold leg: the ladder reads the recorded half, atlas and logs, and says so."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.r = G.measure(scaffold_leg(pathlib.Path(cls.tmp.name) / "leg"), gold=FIX / "gold.json", corpus=FIX / "corpus")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_event_atoms_are_records(self):
        # the same claims as the Entity fixture align the same way when the records are events
        self.assertEqual(self.r["alignment"], {"claims": 10, "claims_aligned": 6, "claims_anchor_ambiguous": 1,
                                               "claims_anchor_absent": 1, "claims_document_not_in_gold": 1,
                                               "claims_not_about_a_record": 1})
        self.assertEqual((self.r["ladder"]["n"], self.r["ladder"]["hit"]), (26, 3))

    def test_the_recorded_atlas_is_read_and_named(self):
        self.assertEqual(self.r["read"]["half"], "recorded")
        self.assertTrue(self.r["atoms"].endswith("recorded/atoms.json"), self.r["atoms"])

    def test_the_trace_stops_at_the_first_replay_step(self):
        self.assertEqual(self.r["ladder"]["documents_read"], 2)
        self.assertEqual(self.r["ladder"]["stages"]["read"]["calls"]["Locate"], {"calls": 18, "per_document": 9.0})
        self.assertIn("replay_extract", self.r["read"]["logs"])
        _, cost = L.trace(pathlib.Path(self.tmp.name) / "leg")
        self.assertEqual(cost["patterns"], {"LOCATED": 2, "READ_LINE": 1, "CALL": 34})

    def test_a_pattern_that_matches_nothing_is_could_not_judge_never_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = scaffold_leg(pathlib.Path(tmp) / "leg", debug_recorded="", job_recorded="")
            _, cost = L.trace(run)
            s = L.summarize([L.Facts(True, True, True)], cost)
        self.assertIsNone(cost["documents_read"])
        self.assertIsNone(cost["calls_by_phase"])
        self.assertEqual(s["stages"]["read"]["calls"], "could_not_judge")

    def test_a_kept_recorded_dir_without_its_atoms_refuses(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = scaffold_leg(pathlib.Path(tmp) / "leg")
            (run / "recorded/atoms.json").unlink()
            r = G.measure(run, gold=FIX / "gold.json", corpus=FIX / "corpus")
        self.assertEqual(r["status"], "never-ran", r.get("reason"))


def sliced_run(root, sections):
    """A run that names a subset of its sections, built from fixtures/gvc-e2e-mini: its index holds one section per
    document (sec_00001 holds A over two chunks, sec_00002 B, sec_00003 C) in chapters.json and chunks.lance, and
    sections.ids (as runs/scaffold-3sys/job.sh writes it) names `sections`."""
    import lance, pyarrow as pa  # noqa: E401, PLC0415  (only this fixture needs them)
    index = root / "data/indexes/cdcr-gvc"
    (index / "atlas").mkdir(parents=True)
    (index / "atlas/atoms.json").write_text((FIX / "run/data/indexes/cdcr-gvc/atlas/atoms.json").read_text())
    chunks = [(1, A), (2, A), (3, B), (4, C)]
    lance.write_dataset(pa.table({"id": [i for i, _ in chunks], "source_doc_id": [d for _, d in chunks]}),
                        str(index / "chunks.lance"))
    (index / "chapters.json").write_text(json.dumps({"chapters": [
        {"id": "sec_00001", "chunk_ids": [1, 2]}, {"id": "sec_00002", "chunk_ids": [3]},
        {"id": "sec_00003", "chunk_ids": [4]}]}))
    for log in ("job.log", "job.debug.log"):
        (root / log).write_text((FIX / "run" / log).read_text())
    (root / "sections.ids").write_text(",".join(sections) + "\n")
    return root


class SlicedRun(unittest.TestCase):
    """A run that names its sections is scored on the gold items whose documents lie in them, and says so."""

    def measure(self, sections):
        with tempfile.TemporaryDirectory() as tmp:
            return G.measure(sliced_run(pathlib.Path(tmp) / "run", sections), gold=FIX / "gold.json", corpus=FIX / "corpus")

    def test_the_sections_map_to_their_documents_through_the_runs_own_index(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = sliced_run(pathlib.Path(tmp) / "run", ["sec_00001", "sec_00003"])
            got = L.run_documents(run, L.run_atlas(run))
        self.assertEqual((got["sections"], got["documents"]), (2, {A, C}))

    def test_items_outside_the_sections_are_left_out_and_counted(self):
        r = self.measure(["sec_00001", "sec_00002"])  # A and B; C's two mentions lie outside
        self.assertEqual((r["ladder"]["n"], r["ladder"]["hit"]), (24, 3))
        self.assertEqual(r["ladder"]["stages"]["read"]["lost"], 14)  # C's two were READ losses
        self.assertEqual(r["scope"], {"sections": 2, "documents": 2, "items": 24, "items_left_out": 2})
        self.assertIn("sections.ids", r["population"])
        self.assertNotIn(C, {x["mention"].split("/")[0] for x in r["items"]})

    def test_identity_scores_only_the_slices_gold(self):
        r = self.measure(["sec_00001", "sec_00002"])
        self.assertEqual((r["identity"]["scored"], r["identity"]["gold"]), (10, 24))
        self.assertEqual(r["identity"]["coverage"], round(10 / 24, 3))

    def test_a_section_the_index_does_not_hold_is_could_not_judge_never_a_smaller_fold(self):
        r = self.measure(["sec_00001", "sec_09999"])
        self.assertEqual(r["status"], "could-not-judge", r.get("reason"))
        self.assertIn("sec_09999", r["reason"])

    def test_a_run_that_names_no_sections_scores_the_whole_fold(self):
        r = G.measure(FIX / "run", gold=FIX / "gold.json", corpus=FIX / "corpus")
        self.assertEqual((r["ladder"]["n"], r["scope"]), (26, None))
        self.assertEqual(r["identity"]["gold"], 26)


RUNS = HERE.parents[1] / "runs/scaffold-3sys"


class ScaffoldRuns(unittest.TestCase):
    """The 2026-10-09 scaffold runs (runs/scaffold-3sys): the uv slice reads the seat's hand post-filter, and the
    whole-fold runs, which list every section of their fold, do not move."""

    def need(self, name):
        if not (RUNS / name).exists():
            self.skipTest(f"could not judge: {RUNS / name} is not on this host")
        return RUNS / name

    def test_the_uv_slice_reads_the_seats_post_filter(self):
        import ladder_uv  # noqa: PLC0415
        r = ladder_uv.measure(self.need("uv-third"))
        self.assertEqual(r["ladder"]["n"], 132)
        self.assertEqual(r["rungs"], {"hit": 30, "no_case_state_claim": 72, "document_not_read": 12,
                                      "wrong_state": 11, "record_not_matched": 7})
        self.assertEqual(r["scope"]["items_left_out"], 386 - 132)
        if not (L.BASELINE / "uv").exists():
            self.skipTest(f"could not judge the like-for-like pair: {L.BASELINE / 'uv'} is not on this host")
        b = ladder_uv.measure(L.BASELINE / "uv", sections_of=RUNS / "uv-third")  # the full-fold run on the slice
        self.assertEqual(b["rungs"], {"hit": 31, "no_case_state_claim": 72, "document_not_read": 12,
                                      "wrong_state": 11, "record_not_matched": 6})
        self.assertEqual(b["identity"]["gold"], r["identity"]["gold"])  # both score the slice's gold

    def test_the_whole_fold_runs_do_not_move(self):
        import ladder_ward  # noqa: PLC0415
        w = ladder_ward.measure(self.need("ward-tune"))
        self.assertEqual((w["ladder"]["n"], w["ladder"]["hit"], w["scope"]["items_left_out"]), (47, 2, 0))
        self.assertEqual([w["ladder"]["stages"][s]["lost"] for s in ("read", "place", "fold")], [19, 25, 1])
        g = G.measure(self.need("gvc"))
        self.assertEqual((g["ladder"]["n"], g["scope"]["items_left_out"]), (977, 0))
        self.assertEqual([(g["ladder"]["stages"][s]["lost"], g["ladder"]["stages"][s]["could_not_judge"])
                          for s in ("read", "place", "fold")], [(180, 0), (484, 0), (0, 313)])

    def test_a_relative_run_path_is_read(self):
        run = self.need("ward-tune")
        r = subprocess.run([sys.executable, str(HERE / "ladder.py"), "--ward", os.path.relpath(run, HERE),
                            "--uv", "none", "--json"], capture_output=True, text=True, cwd=HERE)
        self.assertEqual(r.returncode, 0, r.stderr[-600:])
        self.assertEqual(json.loads(r.stdout.rsplit("\n{\"subject\"", 1)[0])[0]["status"], "judged", r.stdout[-600:])


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


class Vocabulary(unittest.TestCase):
    """A blind run is read through the evaluator's frozen map (ladder.translate), ours through the identity."""
    VOCAB = {"types": {"deal": "deal", "contact": "person"}, "attributes": {"deal": {"status": "stage"}},
             "values": {"deal.status": {"agreed": "won", "flowing": "won"}},
             "kinds": {"deal_step": {"as": "stage_update", "attributes": {"step": "stage"},
                                     "values": {"step": {"offer": "proposal", "correction": None}}},
                       "report": {"as": "case_state", "set": {"state": "reported"}}}}
    ATOMS = [{"atom_type": "Entity", "data": {"id": "d1", "entity_type": "deal", "attributes": {"status": ["agreed", "ended"], "price": "4"}}},
             {"atom_type": "Entity", "data": {"id": "o1", "entity_type": "open_item", "attributes": {}}},
             {"atom_type": "Claim", "data": {"id": "k1", "claim_kind": "deal_step", "subject": "d1",
                                             "attributes": {"step": "offer", "document_id": "m1"}}},
             {"atom_type": "Claim", "data": {"id": "k2", "claim_kind": "deal_step", "subject": "d1", "attributes": {"step": "correction"}}},
             {"atom_type": "Claim", "data": {"id": "k3", "claim_kind": "commitment", "subject": "o1", "attributes": {}}},
             {"atom_type": "Claim", "data": {"id": "k4", "claim_kind": "report", "subject": "d1", "attributes": {}}}]

    def test_the_identity_changes_nothing(self):
        atoms, decs, counts = L.translate(self.ATOMS, [{"type": "deal"}], None)
        self.assertEqual((atoms, decs, counts), (self.ATOMS, [{"type": "deal"}], None))

    def test_names_values_and_constants_map_and_the_rest_can_never_meet_ours(self):
        atoms, decs, counts = L.translate(self.ATOMS, [{"type": "deal", "attribute": "status", "atom": "d1", "values": ["flowing"]}], self.VOCAB)
        d = {a["data"]["id"]: a["data"] for a in atoms}
        self.assertEqual(d["d1"]["attributes"], {"stage": ["won", "unmapped:ended"], "price": "4"})
        self.assertEqual(d["o1"]["entity_type"], "blind:open_item")
        self.assertEqual((d["k1"]["claim_kind"], d["k1"]["attributes"]), ("stage_update", {"stage": "proposal", "document_id": "m1"}))
        self.assertEqual(d["k2"]["attributes"], {"stage": "unmapped:correction"})
        self.assertEqual(d["k3"]["claim_kind"], "blind:commitment")
        self.assertEqual((d["k4"]["claim_kind"], d["k4"]["attributes"]), ("case_state", {"state": "reported"}))
        self.assertEqual(decs, [{"type": "deal", "attribute": "stage", "atom": "d1", "values": ["won"]}])
        self.assertEqual(counts["values_unmapped"], 2)
        self.assertEqual(self.ATOMS[0]["data"]["attributes"]["status"], ["agreed", "ended"])  # the input is not touched

    def test_a_run_finds_its_map_by_its_recipes_round_and_refuses_without_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            t = pathlib.Path(tmp)
            home, maps, run = t / "home", t / "maps", t / "run"
            for p in (home / "blind-author-20261009-r2/ward", maps, run):
                p.mkdir(parents=True)
            rec = home / "blind-author-20261009-r2/ward/recipe.toml"
            rec.write_text("")
            (run / "job.log").write_text(f"x job start: name=w corpus=crm-ward recipe={rec} (ab) bin=cd\n")
            self.assertIsInstance(L.run_vocabulary(run, home, maps), str)  # no map: could-not-judge
            (maps / "r2-mapping.json").write_text(json.dumps({"round": "blind-author-20261009-r2", "systems": {"ward": {"types": {}}}}))
            v = L.run_vocabulary(run, home, maps)
            self.assertEqual((v["round"], v["system"]), ("blind-author-20261009-r2", "ward"))
            (run / "job.log").write_text("x job start: name=w corpus=crm-ward recipe=/elsewhere/crm-ward.toml (ab) bin=cd\n")
            self.assertIsNone(L.run_vocabulary(run, home, maps))  # our recipe: the identity

    def test_the_committed_r2_map_parses_and_names_the_three_systems(self):
        m = json.loads((HERE / "blind-author/r2-mapping.json").read_text())
        self.assertEqual(sorted(m["systems"]), ["gvc", "uv", "ward"])
        self.assertEqual(m["systems"]["gvc"]["resolve_type"], "incident")


if __name__ == "__main__":
    unittest.main()
