"""Held-out integration suite for minilang.

Each test exercises the full tokenize → parse → evaluate pipeline via
the package's public `evaluate`. Tests are grouped by the language
feature they pin.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from minilang import evaluate  # noqa: E402


# ── arithmetic basics ───────────────────────────────────────────────
def test_add():
    assert evaluate("1 + 2") == 3


def test_precedence_mul_over_add():
    assert evaluate("1 + 2 * 3") == 7


def test_parens_override_precedence():
    assert evaluate("(1 + 2) * 3") == 9


def test_integer_division_floors():
    assert evaluate("7 / 2") == 3


def test_multi_digit_numbers():
    assert evaluate("123 + 7") == 130


# ── subtraction left-associativity (parser) ─────────────────────────
def test_subtraction_left_assoc():
    assert evaluate("10 - 3 - 2") == 5


# ── two-char operators (tokenizer) ──────────────────────────────────
def test_le_true():
    assert evaluate("3 <= 3") is True


def test_ge_false():
    assert evaluate("2 >= 5") is False


def test_eq():
    assert evaluate("4 == 4") is True


def test_ne():
    assert evaluate("4 != 5") is True


# ── power: right-assoc + precedence vs unary minus (parser) ──────────
def test_power_basic():
    assert evaluate("2 ** 3") == 8


def test_power_right_assoc():
    assert evaluate("2 ** 3 ** 2") == 512


def test_unary_minus_below_power():
    # -3 ** 2 == -(3 ** 2) == -9
    assert evaluate("-3 ** 2") == -9


def test_unary_minus_basic():
    assert evaluate("-5 + 2") == -3


# ── keywords / booleans (tokenizer + evaluator) ─────────────────────
def test_or_keyword():
    assert evaluate("false or true") is True


def test_and_keyword():
    assert evaluate("true and false") is False


def test_not():
    assert evaluate("not (3 < 2)") is True


# ── short-circuit evaluation (evaluator) ────────────────────────────
def test_and_short_circuits_past_div_by_zero():
    # Right operand would raise; `and` must not evaluate it.
    assert evaluate("false and (10 / 0 == 0)") is False


def test_or_short_circuits_past_div_by_zero():
    assert evaluate("true or (10 / 0 == 0)") is True


# ── let / lexical scope (evaluator) ─────────────────────────────────
def test_let_basic():
    assert evaluate("let x = 5 in x * x") == 25


def test_let_lexical_scope():
    # Inner shadow must not leak past its body; outer x stays 10.
    assert evaluate("let x = 10 in (let x = 1 in 0) + x") == 10


def test_let_nested_uses_outer():
    assert evaluate("let x = 3 in let y = 4 in x * y") == 12


# ── booleans interplay ──────────────────────────────────────────────
def test_comparison_then_and():
    assert evaluate("(1 < 2) and (3 != 4)") is True


def test_let_with_comparison_body():
    assert evaluate("let n = 7 in n >= 7") is True
