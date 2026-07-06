"""Smoke subset for minilang (iterate against this in the workdir).

The full held-out suite replaces this file at grading time. These
eight cases span all three stage files so a failing run points you at
every layer that needs work.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from minilang import evaluate  # noqa: E402


def test_add():
    assert evaluate("1 + 2") == 3


def test_integer_division_floors():
    assert evaluate("7 / 2") == 3


def test_subtraction_left_assoc():
    assert evaluate("10 - 3 - 2") == 5


def test_le_true():
    assert evaluate("3 <= 3") is True


def test_power_right_assoc():
    assert evaluate("2 ** 3 ** 2") == 512


def test_or_keyword():
    assert evaluate("false or true") is True


def test_and_short_circuits_past_div_by_zero():
    assert evaluate("false and (10 / 0 == 0)") is False


def test_let_lexical_scope():
    assert evaluate("let x = 10 in (let x = 1 in 0) + x") == 10
