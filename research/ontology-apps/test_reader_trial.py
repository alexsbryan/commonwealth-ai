import contextlib
import io
import pathlib
import sys
import tempfile
import tomllib
import unittest


APP = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(APP))
import reader_trial as trial  # noqa: E402
import reader_trial_data as trial_data  # noqa: E402
import reader_trial_scoring as trial_scoring  # noqa: E402


class ReaderTrialTests(unittest.TestCase):
    def test_recipe_arms_preserve_original_declarations_and_only_toggle_reading(self):
        with tempfile.TemporaryDirectory() as tmp:
            for app, recipe_dir, original_id in (("uv", "support", "uv-support"), ("ward", "ward", "crm-ward")):
                original = tomllib.loads((APP / recipe_dir / "recipe.toml").read_text())
                rendered = [
                    tomllib.loads(trial.render_recipe(app, f"{app}-arm-{flag}", pathlib.Path(tmp) / app, flag))
                    for flag in (False, True)
                ]
                self.assertEqual(original["corpus"]["id"], original_id)
                for recipe, expected in zip(rendered, (False, True)):
                    self.assertEqual(recipe["enrichment"]["ontology"]["document_reading"], expected)
                    self.assertEqual(recipe["enrichment"]["ontology"]["types"][:-1], original["enrichment"]["ontology"]["types"])
                self.assertEqual(rendered[0]["enrichment"]["ontology"]["types"][-1], rendered[1]["enrichment"]["ontology"]["types"][-1])

    def test_document_read_statuses_come_only_from_cached_outcomes(self):
        cached = {
            "questions_by_chapter": [{
                "section_extraction": {
                    "document_read": {
                        "documents": [
                            {"document_id": "a", "status": "read"},
                            {"document_id": "b", "status": "could_not_judge"},
                        ]
                    }
                }
            }]
        }
        outcomes, refused = trial_scoring.document_read_outcomes(cached)
        self.assertEqual(outcomes, {"a": "read", "b": "could_not_judge"})
        self.assertEqual(refused, {})
        absent = trial_scoring.read_status_report(pathlib.Path("/path/that/does/not/exist"), "uv", ["a"], pathlib.Path("."))
        self.assertEqual(absent["status"], "not_recorded")
        self.assertEqual(absent["selected_document_statuses"], {})

    def test_invoke_archives_nonzero_exit_without_success_shape(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = pathlib.Path(tmp) / "step.log"
            quiet = io.StringIO()
            with contextlib.redirect_stdout(quiet):
                result = trial.invoke(
                    "controlled-failure",
                    [sys.executable, "-c", "import sys; print('out'); print('err', file=sys.stderr); sys.exit(7)"],
                    log,
                )
            self.assertEqual(result["exit_code"], 7)
            self.assertNotEqual(result["exit_code"], 0)
            self.assertIn("out", log.read_text())
            self.assertIn("err", log.read_text())
            self.assertIn("elapsed_seconds=", log.read_text())

    def test_existing_root_is_never_reused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp) / "immutable"
            root.mkdir()
            with self.assertRaises(FileExistsError):
                trial_data.validate_fresh_targets(root)


if __name__ == "__main__":
    unittest.main()
