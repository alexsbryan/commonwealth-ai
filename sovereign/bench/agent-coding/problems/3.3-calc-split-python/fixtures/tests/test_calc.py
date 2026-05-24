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
