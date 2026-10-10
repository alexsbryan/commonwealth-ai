"""bars.py's tests: one fixture per selection rule, the frozen-recipe runs never qualifying, and the count unit.

    python3 test_bars.py
"""
import contextlib, io, json, os, pathlib, shutil, sys, tempfile, time, unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True
import bars as B  # noqa: E402

FIX = HERE / "fixtures/bars"
BASELINE_RECIPES = pathlib.Path.home() / ".svrnmesh/bench-corpora/baseline-3sys-20261009/recipes"
FROZEN = {"gvc": "cdcr-gvc.toml", "ward": "crm-ward.toml", "uv": "uv-support.toml"}
REAL_FROZEN_RUNS = [B.RUNS / "scaffold-3sys", B.RUNS / "e2-gvc-resolve-alone"]


def run_quiet(fn, *a, **kw):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        fn(*a, **kw)
    return json.loads(out.getvalue().strip().splitlines()[-1]), err.getvalue()


def leg(runs, run, name, corpus, recipe, sections=50, resolve_exit=0, age=0):
    r = runs / run
    r.mkdir(parents=True, exist_ok=True)
    (r / "job.sh").write_text("#!/bin/sh\n")
    d = r / name
    d.mkdir()
    (d / "job.log").write_text(f"2026-10-09T00:00:00Z job start: name={name} corpus={corpus} recipe={recipe} "
                               f"(abc) bin=def data_root={d}/data\n")
    (d / "DONE").write_text(f"sections={sections}\npublish_exit=0\nresolve_exit={resolve_exit}\n")
    t = time.time() - age
    os.utime(d / "DONE", (t, t))
    return d


def judged(identity=0.5):
    seen = []

    def judge(system, lg):
        seen.append(lg["name"])
        return {"system": system, "status": "judged", "identity": {"b_cubed": identity}}
    return judge, seen


class Tmp(unittest.TestCase):
    def setUp(self):
        self._t = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self._t.name)
        self.runs, self.home = self.root / "runs", self.root / "home"
        self.runs.mkdir()
        self.home.mkdir()
        self.frozen = str(self.root / "bench/recipes/crm-ward.toml")

    def tearDown(self):
        self._t.cleanup()

    def blind(self, rnd, system, corpus_id="x"):
        d = self.home / rnd / system
        d.mkdir(parents=True, exist_ok=True)
        (d / "recipe.toml").write_text(f'[corpus]\nid = "{corpus_id}"\n')
        (d / "recipe-author.toml").write_text('[skill]\nid = "recipe-author"\n')
        return d / "recipe.toml"


class DefaultPath(Tmp):
    def test_absent_without_a_job_run(self):
        with self.assertRaises(B.Absent):
            B.default_path(self.runs)

    def test_counts_the_largest_non_smoke_leg_per_corpus_of_the_newest_run(self):
        for name, corpus, n in (("gvc", "cdcr-gvc", 77), ("ward-tune", "crm-ward", 81), ("ward-smoke", "crm-ward", 5),
                                ("uv-third", "uv-support", 61), ("uv-smoke", "uv-support", 5)):
            leg(self.runs, "new", name, corpus, self.frozen, sections=n, age=10)
        for name, corpus in (("gvc", "cdcr-gvc"), ("ward", "crm-ward"), ("uv", "uv-support")):
            leg(self.runs, "old", name, corpus, self.frozen, age=1000)
        judge, seen = judged()
        row, _ = run_quiet(B.default_path, self.runs, judge)
        self.assertEqual(row["value"], 3)
        self.assertTrue(row["artifact"].endswith("/new"))
        self.assertEqual(sorted(seen), ["gvc", "uv-third", "ward-tune"])

    def test_a_smoke_leg_alone_and_a_failed_resolve_do_not_count(self):
        leg(self.runs, "r", "ward-smoke", "crm-ward", self.frozen)
        leg(self.runs, "r", "uv", "uv-support", self.frozen, resolve_exit=1)
        leg(self.runs, "r", "gvc", "cdcr-gvc", self.frozen)
        judge, seen = judged()
        row, err = run_quiet(B.default_path, self.runs, judge)
        self.assertEqual(row["value"], 1)
        self.assertEqual(seen, ["gvc"])
        self.assertIn("ward: no non-smoke leg", err)
        self.assertIn("resolve_exit=1", err)

    def test_a_ladder_that_does_not_judge_does_not_count(self):
        leg(self.runs, "r", "gvc", "cdcr-gvc", self.frozen)
        row, _ = run_quiet(B.default_path, self.runs, lambda s, l: {"status": "could-not-judge", "reason": "x"})
        self.assertEqual(row["value"], 0)


class IdentityLegs(Tmp):
    def test_a_frozen_recipe_leg_never_qualifies(self):
        leg(self.runs, "r", "ward-tune", "crm-ward", self.frozen)
        with self.assertRaises(B.Absent):
            B.identity_leg("ward", self.runs, self.home, judged()[0])

    def test_the_material_dir_is_not_a_round(self):
        material = self.home / "blind-author-material-20261009/ward/recipe.toml"
        leg(self.runs, "r", "ward-tune", "crm-ward", material)
        with self.assertRaises(B.Absent):
            B.identity_leg("ward", self.runs, self.home, judged()[0])

    def test_a_blind_leg_reads_ladder_b3_and_a_smoke_one_never(self):
        rec = self.blind("blind-author-20261009-r2", "uv")
        leg(self.runs, "r", "uv-smoke", "uv-support", rec, sections=5)
        with self.assertRaises(B.Absent):
            B.identity_leg("uv", self.runs, self.home, judged()[0])
        leg(self.runs, "r", "uv-tune", "uv-support", rec, sections=60)
        leg(self.runs, "r", "ward-tune", "crm-ward", self.frozen, sections=90)
        judge, seen = judged(0.91)
        row, _ = run_quiet(B.identity_leg, "uv", self.runs, self.home, judge)
        self.assertEqual((row["value"], seen), (0.91, ["uv-tune"]))

    def test_a_blind_leg_whose_resolve_failed_cannot_be_judged(self):
        leg(self.runs, "r", "ward-tune", "crm-ward", self.blind("blind-author-20261010", "ward"), resolve_exit=1)
        with self.assertRaises(B.CannotJudge):
            B.identity_leg("ward", self.runs, self.home, judged()[0])


class IdentityGvc(Tmp):
    def resolve_run(self, name, recipe, conll=0.61, er=True):
        r = self.runs / name
        (r / "resolve").mkdir(parents=True)
        (r / "resolve/summary.json").write_text(json.dumps({"recipe": str(recipe)}))
        if er:
            (r / "er-score.json").write_text(json.dumps({"conll_f1": conll}))
        return r

    def test_a_frozen_recipe_run_never_qualifies(self):
        self.resolve_run("e2", self.root / "bench/recipes/cdcr-gvc.toml")
        with self.assertRaises(B.Absent):
            B.identity_gvc(self.runs, self.home)

    def test_a_blind_run_without_er_score_is_absent_and_with_one_reads_conll(self):
        rec = self.blind("blind-author-20261009-r2", "gvc")
        self.resolve_run("blind", rec, er=False)
        with self.assertRaises(B.Absent):
            B.identity_gvc(self.runs, self.home)
        (self.runs / "blind/er-score.json").write_text(json.dumps({"conll_f1": 0.6123456}))
        self.resolve_run("frozen", self.root / "bench/recipes/cdcr-gvc.toml", conll=0.99)
        row, _ = run_quiet(B.identity_gvc, self.runs, self.home)
        self.assertEqual(row["value"], 0.6123)
        self.assertTrue(row["artifact"].endswith("blind/er-score.json"))

    def test_a_blind_ward_recipe_does_not_qualify_for_gvc(self):
        self.resolve_run("x", self.blind("blind-author-20261009-r2", "ward"))
        with self.assertRaises(B.Absent):
            B.identity_gvc(self.runs, self.home)


class RealFrozenRunsNeverQualify(unittest.TestCase):
    """Our frozen-recipe runs on this host: every recipe they name maps to no blind system."""

    def test_every_leg_and_resolve_summary_of_the_frozen_runs(self):
        named = []
        for run in REAL_FROZEN_RUNS:
            if not run.exists():
                self.skipTest(f"{run} is not on this host")
            named += [l["recipe"] for d in run.iterdir() if d.is_dir() for l in [B.leg_of(d)] if l]
            named += [json.loads(p.read_text())["recipe"] for p in run.rglob("summary.json")
                      if "recipe" in json.loads(p.read_text())]
        self.assertGreaterEqual(len(named), 7)  # 5 scaffold legs + 2 resolve-alone summaries
        self.assertEqual([r for r in named if B.blind_system(r)], [])


class Estimator(Tmp):
    def scored(self, name, recipe, gap):
        r = self.runs / name
        (r / "resolve").mkdir(parents=True)
        (r / "resolve/summary.json").write_text(json.dumps({"recipe": str(recipe)}))
        row = json.loads((FIX / "score_resolve.gvc.json").read_text())
        row["run"] = str(r / "resolve")
        row["estimator"]["largest_gap"] = [gap, "proposed_answer"]
        (r / "score_resolve.json").write_text(json.dumps(row))

    def frozen_recipe(self, corpus):
        p = self.root / f"bench/{corpus}.toml"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(f'[corpus]\nid = "{corpus}"\n')
        return p

    def test_absent_naming_the_systems_without_a_block(self):
        self.scored("g", self.frozen_recipe("cdcr-gvc"), 0.449)
        with self.assertRaises(B.Absent) as e:
            B.estimator(self.runs, self.home)
        self.assertIn("ward, uv", str(e.exception))

    def test_the_greatest_gap_over_the_three_systems(self):
        self.scored("g", self.frozen_recipe("cdcr-gvc"), 0.449)
        self.scored("w", self.frozen_recipe("crm-ward"), 0.2)
        self.scored("u", self.blind("blind-author-20261009-r2", "uv", corpus_id="whatever"), 0.6)
        row, _ = run_quiet(B.estimator, self.runs, self.home)
        self.assertEqual(row["value"], 0.6)
        self.assertTrue(row["artifact"].endswith("u/score_resolve.json"))


class NoTuning(Tmp):
    def test_the_count_unit_is_13_over_the_frozen_baseline_recipes(self):
        if not BASELINE_RECIPES.is_dir():
            self.skipTest(f"{BASELINE_RECIPES} is not on this host")
        import tomllib  # noqa: PLC0415
        per = {s: B.count_tuning(tomllib.loads((BASELINE_RECIPES / f).read_text())) for s, f in FROZEN.items()}
        self.assertEqual(per, {"gvc": 3, "ward": 4, "uv": 6})  # 3+2+4 evidential entries; 0+2+2 reader keys
        self.assertEqual(sum(per.values()), 13)

    def test_an_entry_is_one_field_and_a_switch_key_is_one_field_at_any_depth(self):
        doc = {"enrichment": {"ontology": {"document_reading": False, "document_reader": "passes", "types": [
            {"identity_evidential": [{"right": 1, "of": 2}, {"right": 3, "of": 4}]}, {"identity_evidential": []}]}}}
        self.assertEqual(B.count_tuning(doc), 4)

    def test_the_newest_round_and_its_recipe_toml(self):
        for s in B.SYSTEMS:
            self.blind("blind-author-20261009", s)
            self.blind("blind-author-material-20261010", s)
            p = self.blind("blind-author-20261009-r2", s)
        p.write_text('[corpus]\nid = "u"\n[enrichment.ontology]\ndocument_reader = "passes"\n')
        row, _ = run_quiet(B.no_tuning, self.home)
        self.assertEqual(row["value"], 1)
        self.assertTrue(row["artifact"].endswith("blind-author-20261009-r2"))

    def test_two_recipe_shaped_files_cannot_be_judged_and_a_missing_system_is_absent(self):
        with self.assertRaises(B.Absent):
            B.no_tuning(self.home)
        for s in ("gvc", "ward"):
            self.blind("blind-author-20261009", s)
        with self.assertRaises(B.Absent):
            B.no_tuning(self.home)
        (self.blind("blind-author-20261009", "uv").parent / "recipe-author.toml").write_text('[corpus]\nid = "x"\n')
        with self.assertRaises(B.CannotJudge):
            B.no_tuning(self.home)


class Invariants(unittest.TestCase):
    def test_counts_contracts_with_a_pass_and_no_failure(self):
        self.assertEqual(B.contracts_held(FIX / "cargo.jsonl"), 6)
        self.assertEqual(B.contracts_held(FIX / "cargo-c3-fails.jsonl"), 5)
        self.assertIsNone(B.contracts_held(FIX / "cargo-no-contracts.jsonl"))

    def test_a_run_that_did_not_rewrite_the_jsonl_cannot_be_judged(self):
        with tempfile.TemporaryDirectory() as t:
            j = pathlib.Path(t) / "cargo.jsonl"
            shutil.copy(FIX / "cargo.jsonl", j)
            os.utime(j, (0, 0))
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(B.CannotJudge):
                B.invariants(j, ["true"])
            with self.assertRaises(B.CannotJudge):
                B.invariants(pathlib.Path(t) / "absent.jsonl", ["true"])

    def test_a_rewritten_jsonl_is_counted(self):
        with tempfile.TemporaryDirectory() as t:
            j = pathlib.Path(t) / "cargo.jsonl"
            row, _ = run_quiet(B.invariants, j, ["cp", str(FIX / "cargo-c3-fails.jsonl"), str(j)])
            self.assertEqual(row["value"], 5)


class Exits(unittest.TestCase):
    def test_absent_exits_3_cannot_judge_1_unknown_bar_2(self):
        def absent():
            raise B.Absent("nothing")

        def cnj():
            raise B.CannotJudge("broken")
        saved = dict(B.READERS)
        try:
            B.READERS.update({"a": absent, "c": cnj})
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(B.main(["bars.py", "a"]), 3)
                self.assertEqual(B.main(["bars.py", "c"]), 1)
                self.assertEqual(B.main(["bars.py", "layer-author-gap"]), 2)
        finally:
            B.READERS.clear()
            B.READERS.update(saved)


if __name__ == "__main__":
    unittest.main()
