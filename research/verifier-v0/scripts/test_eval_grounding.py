#!/usr/bin/env python3
"""Verdict-parser tests for eval_grounding.py.

Run: .venv/bin/python scripts/test_eval_grounding.py   (exit 0 = pass)

These exist because on 2026-08-02 the 0.8B probe checkpoint scored 0/4 on a
smoke eval while producing *correct verdicts*. The strict ANSWER_RE requires a
fully well-formed <answer> block, so a right answer with a typo'd closing tag
was discarded as a parse failure. Same rule cost the 4B baseline 8.6% of rows
and ~6 BAcc points (BASELINES.md: strict 70.77 vs excl-pf 76.76).

The two negative cases at the bottom are the ones that matter most: tolerance
must never turn "the model discussed the categories" into "the model answered."
"""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_grounding import parse_verdict, parse_verdict_tolerant  # noqa: E402

# (name, text, strict_cls, tolerant_pred, tolerant_how)
CASES = [
    # --- shapes seen in real runs -------------------------------------------
    ("well-formed (HalluGuard-4B baseline shape)",
     "<think>x</think><answer><classification>GROUNDED</classification>"
     "<justification>because</justification></answer>", "GROUNDED", 1, "strict"),

    ("0.8B probe: typo'd </justivation>, no </answer>",
     "<think>x</think><answer>\n<classification>GROUNDED</classification>\n"
     "<justification>Your reasoning here: matches.</justivation>", None, 1, "tag"),

    ("0.8B probe --no-think: JSON body inside <answer>",
     '<answer>\n{"classification":"GROUNDED","justification":"supported."}',
     None, 1, "json"),

    ("hallucinated, bare tag",
     "<think>y</think><classification>HALLUCINATED_EXTRINSIC</classification>",
     None, 0, "tag"),

    ("repetition loop: hit the token cap still inside <think>",
     "<think>Is it supported? Yes. " * 50 + "</think>", None, None, None),

    # --- ordering ------------------------------------------------------------
    ("two tags after </think> -- last one wins (matches strict parser)",
     "<think>x</think><classification>GROUNDED</classification>"
     "<classification>HALLUCINATED_INTRINSIC</classification>", None, 0, "tag"),

    ("invalid category token is not a verdict",
     "<think>x</think><classification>MAYBE</classification>", None, None, None),

    # --- the two that guard against false tolerance --------------------------
    ("template quoted INSIDE <think> must not leak a verdict",
     "<think>CATEGORY must be ONE of: GROUNDED, HALLUCINATED_INTRINSIC. "
     "<classification>GROUNDED</classification></think>Sorry, I cannot.",
     None, None, None),

    ("bare prose mention after </think> is not an answer",
     "<think>x</think>I think the claim is GROUNDED, honestly.", None, None, None),
]


def main() -> int:
    failed = 0
    for name, text, want_strict, want_pred, want_how in CASES:
        _, got_strict = parse_verdict(text)
        got_pred, _, got_how = parse_verdict_tolerant(text)
        ok = got_strict == want_strict and got_pred == want_pred and got_how == want_how
        failed += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        if not ok:
            print(f"       strict={got_strict!r} want {want_strict!r} | "
                  f"tolerant=({got_pred!r},{got_how!r}) want ({want_pred!r},{want_how!r})")
    print(f"\n{len(CASES) - failed}/{len(CASES)} passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
