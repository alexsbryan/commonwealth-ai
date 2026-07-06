import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from calc import evaluate, solve_linear, statistics


def test_evaluate_addition():
    assert evaluate("1 + 2") == 3.0


def test_evaluate_precedence():
    assert evaluate("2 + 3 * 4") == 14.0


def test_evaluate_parens():
    assert evaluate("(2 + 3) * 4") == 20.0


def test_solve_linear_normal():
    assert solve_linear(2, -10) == 5.0


def test_solve_linear_zero_coefficient():
    assert solve_linear(0, 0) == 0
    assert solve_linear(0, 5) is None


def test_statistics():
    mean, var, std = statistics([1, 2, 3, 4, 5])
    assert mean == 3.0
    assert abs(var - 2.0) < 1e-9
    assert abs(std - 1.41421356) < 1e-5


def test_every_source_file_within_30_lines():
    # The task's structural goal, straight from the prompt: "split
    # calc.py into source files where every source file is <= 30
    # lines". Encoded as a test so the fitness signal covers the
    # WHOLE goal — without it the behavior tests pass at baseline
    # and a maximize-passing solver correctly does nothing. The
    # grader's held-out suite replaces this file at witness time, so
    # correctness scoring stays behavior-only; this test exists for
    # the iteration loop.
    root = os.path.dirname(os.path.dirname(__file__))
    offenders = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in ("tests", "__pycache__", ".git")]
        for name in filenames:
            if name.endswith(".py"):
                path = os.path.join(dirpath, name)
                with open(path) as fh:
                    count = sum(1 for _ in fh)
                if count > 30:
                    offenders.append(f"{os.path.relpath(path, root)}: {count} lines")
    assert not offenders, f"source files over the 30-line budget: {offenders}"
