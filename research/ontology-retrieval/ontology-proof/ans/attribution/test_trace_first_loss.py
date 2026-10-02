"""The first-loss instrument catches a planted dropped passage, not a title."""

import hashlib
import json
import unittest

from trace_first_loss import fingerprint, first_loss, parse_trace, prompt_verdict


class TraceFirstLossTests(unittest.TestCase):
    def test_planted_cap_drop_names_the_cap_step(self):
        question = "Which section answers this?"
        qid = hashlib.sha256(question.encode()).hexdigest()[:12]
        gold = fingerprint("ans", "INTRODUCTION", "the answer is here")
        other = fingerprint("ans", "INTRODUCTION", "different text, same title")

        def event(name, before, after):
            return (
                f'retrieval.pipeline: passage identities pipeline="knowledge_query" '
                f'step="{name}" query_hash={qid} before={json.dumps(before)} '
                f'after={json.dumps(after)}'
            )

        trace = "\n".join((
            event("main_retrieval_mesh", [], [gold]),
            event("atlas_grounding", [gold], [gold, other]),
            event("cap_and_reserve", [gold, other], [other]),
            event("scope_audit", [other], [other]),
        ))
        steps = parse_trace(trace, question)
        self.assertEqual(len(steps), 4)
        self.assertEqual(first_loss(steps, gold), "lost at cap_and_reserve")
        self.assertIn("survived tracked pipeline", first_loss(steps, other))

    def test_a_missing_step_or_wrong_question_never_proves_a_loss(self):
        q = "question"
        self.assertTrue(first_loss(parse_trace("", q), "gold").startswith("could-not-judge"))
        self.assertTrue(first_loss([("main_retrieval_mesh", [], ["gold"])], "gold").startswith("could-not-judge"))

    def test_reintroduced_passage_is_not_a_final_loss(self):
        steps = [
            ("main_retrieval_mesh", [], ["gold"]),
            ("noise_floor", ["gold"], []),
            ("atlas_grounding", [], ["gold"]),
            ("scope_audit", ["gold"], ["gold"]),
        ]
        self.assertIn("survived tracked pipeline", first_loss(steps, "gold"))
        steps[-1] = ("scope_audit", [], [])  # a missing handoff cannot prove a loss
        self.assertTrue(first_loss(steps, "gold").startswith("could-not-judge"))

    def test_formatter_boundary_requires_admission_record(self):
        row = {"question": "q", "retrieved": [{"in_prompt": False, "prompt_text": None}]}
        self.assertIn("absent from recorded prompt", prompt_verdict({"results": [row]}, "q", "gold"))
        row["retrieved"][0] = {"in_prompt": True, "prompt_text": "gold here"}
        self.assertIn("reached the recorded prompt", prompt_verdict({"results": [row]}, "q", "gold"))
        row["retrieved"][0] = {"snippet": "gold", "in_prompt": None}
        self.assertTrue(prompt_verdict({"results": [row]}, "q", "gold").startswith("could-not-judge"))

    def test_downstream_expansion_and_admission_are_real_loss_boundaries(self):
        stages = [
            ("main_retrieval_mesh", [], ["gold"]),
            ("scope_audit", ["gold"], ["gold"]),
            ("cohesion_expansion", ["gold"], []),
            ("conv_ppr_rerank", [], []),
            ("late_summaries", [], []),
            ("agentic_evidence", [], []),
            ("prompt_admission", [], []),
        ]
        self.assertEqual(first_loss(stages, "gold"), "lost at cohesion_expansion")
        stages[2] = ("cohesion_expansion", ["gold"], ["gold"])
        stages[3] = ("conv_ppr_rerank", ["gold"], ["gold"])
        stages[4] = ("late_summaries", ["gold"], ["gold"])
        stages[5] = ("agentic_evidence", ["gold"], ["gold"])
        stages[6] = ("prompt_admission", ["gold"], [])
        self.assertEqual(first_loss(stages, "gold"), "lost at prompt_admission")


if __name__ == "__main__":
    unittest.main()
