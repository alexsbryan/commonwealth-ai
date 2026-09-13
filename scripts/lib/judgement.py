#!/usr/bin/env python3
"""The trailing-Judgement-line protocol, for the tools written in Python.

A tool that decides a four-verdict outcome (ARCH §18.2) should be able to SAY
which one it reached, in the vocabulary the product's own gate uses, instead of
having its prose grepped. `sovereign-cli-shared/src/lane_verdict.rs` is the
protocol and its module header names the cost of the alternative:
`scripts/lib/ci-bench-verdict.sh` reconstructs a four-verdict decision by
grepping a lane's output for "N regressed" and "unmeasured — every question
errored", and "every one of those greps is a coupling to wording nobody
promised to keep".

The line is the LAST non-empty line of stdout:

    {"subject":"twin-census","verdict":"failed","reason":"2 of 31 plants survived","as_of":1789...}

WHY A SECOND IMPLEMENTATION EXISTS, AND WHAT HOLDS IT TO THE FIRST.  The
protocol is a wire format and is language-agnostic on purpose — a Python tool
cannot call `lane_verdict::emit`, and nine copies of `json.dumps` would be nine
places for the shape to drift. So this is ONE mirror, not nine, and it is tied
to the original by a test that parses this file's own `--selftest` output with
the Rust parser (`sovereign-cli-shared`, `the_python_emitter_speaks_the_same_
protocol`). Change either side without the other and that test goes red.

Use it as a module:

    from judgement import emit
    emit("twin-census", "failed", f"{n} of {total} plants survived")

or as a CLI, which is how the shell tools reach it without hand-rolling JSON
escaping:

    python3 scripts/lib/judgement.py --subject ci-bench --verdict passed \
        --reason "12 lanes, none regressed"
"""

from __future__ import annotations

import argparse
import json
import sys
import time

#: The four wire spellings, mirroring `kernel_types::Verdict::parse_wire`.
#: A fifth word is not a verdict this protocol can carry — refused here rather
#: than mapped, because an unrecognised verdict is an absence (ARCH §18.3).
VERDICTS = ("passed", "failed", "could-not-judge", "never-ran")

#: Reasons that carry no information, mirroring `kernel_types`' PLACEHOLDERS.
#: EXACT match after trim+lowercase, deliberately: prose beginning "unknown
#: cause of the flap" is informative.
PLACEHOLDERS = frozenset(
    ["", "-", "--", "?", "n/a", "na", "none", "null", "nil", "tbd", "todo",
     "unknown", "unspecified"]
)


def render(subject: str, verdict: str, reason: str, as_of: float | None = None) -> str:
    """The line, including its trailing newline.

    Raises `ValueError` on a verdict outside the four or a reason that carries
    no information — the same refusal `Reason::literal` makes by panicking, and
    for the same reason: both are the tool author's mistake, visible on the
    first run rather than as a row nobody can act on.
    """
    if verdict not in VERDICTS:
        raise ValueError(
            f"verdict {verdict!r} is not one of {'/'.join(VERDICTS)} — "
            "an unrecognised verdict is refused, never mapped"
        )
    if reason.strip().lower() in PLACEHOLDERS:
        raise ValueError(
            f"reason {reason!r} carries no information. Say what was checked "
            "and what it showed; the reason is the half a reader acts on"
        )
    obj = {"subject": subject, "verdict": verdict, "reason": reason}
    if as_of is not None:
        obj["as_of"] = int(as_of)
    return json.dumps(obj, separators=(",", ":")) + "\n"


def emit(subject: str, verdict: str, reason: str, as_of: float | None = None) -> None:
    """Print the line and nothing after it. `as_of` defaults to now."""
    sys.stdout.write(render(subject, verdict, reason, time.time() if as_of is None else as_of))
    sys.stdout.flush()


def _selftest() -> int:
    """Emit one line per verdict, then one refusal per rule, for the Rust test.

    The refusals print to stdout as `REFUSED <text>` so the test can assert the
    mirror refuses what the original refuses — a mirror that only agreed on the
    happy path would drift on exactly the cases the strictness is for.
    """
    for v in VERDICTS:
        sys.stdout.write(render("selftest", v, f"the {v} arm of the selftest", 1788560000))
    for bad_verdict in ("green", "ok", "PASSED"):
        try:
            render("selftest", bad_verdict, "a real reason")
        except ValueError:
            sys.stdout.write(f"REFUSED verdict {bad_verdict}\n")
    for bad_reason in ("", "  ", "unknown", "TBD", "n/a"):
        try:
            render("selftest", "passed", bad_reason)
        except ValueError:
            sys.stdout.write(f"REFUSED reason {bad_reason!r}\n")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--subject")
    ap.add_argument("--verdict", choices=VERDICTS)
    ap.add_argument("--reason")
    ap.add_argument("--as-of", type=float, default=None)
    ap.add_argument("--selftest", action="store_true",
                    help="emit one line per verdict plus the refusals, for the protocol test")
    a = ap.parse_args(argv)
    if a.selftest:
        return _selftest()
    if not (a.subject and a.verdict and a.reason):
        ap.error("--subject, --verdict and --reason are all required")
    try:
        # Stamped by default, like `emit`: a verdict with no date cannot be
        # read for staleness later, and every caller would otherwise have to
        # remember to pass one.
        sys.stdout.write(render(a.subject, a.verdict, a.reason,
                                time.time() if a.as_of is None else a.as_of))
    except ValueError as e:
        print(f"judgement: {e}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
