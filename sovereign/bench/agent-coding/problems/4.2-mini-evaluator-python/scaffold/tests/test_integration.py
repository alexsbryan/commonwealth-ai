"""Held-out integration tests for 4.2 mini-evaluator (Python).

20 tests organized by stage so the model can map failures back to
the buggy stage from the pytest output alone.

Bug map:
  1. tokenize: <=, >=, ==, != mis-lexed
  2. parse:    ** parsed left-associative (should be right)
  3. parse:    unary - binds looser than ** (should bind tighter)
  4. evaluate: and/or evaluate both operands (should short-circuit)
  5. evaluate: let bindings leak (should be lexical)
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from evaluator import (  # noqa: E402
    BinOp,
    BoolLit,
    IntLit,
    UnaryOp,
    parse,
    run,
    tokenize,
)


# ── Lexer (5 tests) ──────────────────────────────────────────────


def test_lex_integers_and_operators():
    toks = tokenize("12 + 34 * 5")
    kinds = [t.kind for t in toks]
    assert kinds == ["INT", "PLUS", "INT", "STAR", "INT"]
    values = [t.value for t in toks]
    assert values == [12, "+", 34, "*", 5]


def test_lex_keywords_vs_idents():
    toks = tokenize("if x then 1 else 0")
    kinds = [t.kind for t in toks]
    assert kinds == ["IF", "IDENT", "THEN", "INT", "ELSE", "INT"]


def test_lex_two_char_comparison_operators_le_ge_eq_neq():
    # Bug #1 lands here: <=, >=, ==, != must be SINGLE tokens.
    toks = tokenize("a <= b >= c == d != e")
    kinds = [t.kind for t in toks]
    assert kinds == [
        "IDENT", "LE",
        "IDENT", "GE",
        "IDENT", "EQ",
        "IDENT", "NEQ",
        "IDENT",
    ]


def test_lex_pow_is_two_char():
    toks = tokenize("2 ** 3")
    assert [t.kind for t in toks] == ["INT", "POW", "INT"]


def test_lex_rejects_unknown_char():
    with pytest.raises(SyntaxError):
        tokenize("1 @ 2")


# ── Parser (5 tests) ─────────────────────────────────────────────


def test_parse_left_associative_addition():
    ast = parse(tokenize("1 + 2 + 3"))
    # Expected shape: BinOp("+", BinOp("+", 1, 2), 3)
    assert isinstance(ast, BinOp) and ast.op == "+"
    assert isinstance(ast.left, BinOp) and ast.left.op == "+"
    assert isinstance(ast.right, IntLit) and ast.right.value == 3


def test_parse_multiplication_higher_precedence_than_addition():
    ast = parse(tokenize("1 + 2 * 3"))
    # Expected: BinOp("+", 1, BinOp("*", 2, 3))
    assert isinstance(ast, BinOp) and ast.op == "+"
    assert isinstance(ast.left, IntLit) and ast.left.value == 1
    assert isinstance(ast.right, BinOp) and ast.right.op == "*"


def test_parse_power_is_right_associative():
    # Bug #2: ** must be right-associative.
    # Expected shape: BinOp("**", 2, BinOp("**", 3, 2))
    ast = parse(tokenize("2 ** 3 ** 2"))
    assert isinstance(ast, BinOp) and ast.op == "**"
    assert isinstance(ast.left, IntLit) and ast.left.value == 2
    assert isinstance(ast.right, BinOp) and ast.right.op == "**"


def test_parse_unary_minus_binds_tighter_than_power():
    # Bug #3: -3 ** 2 must parse as -(3 ** 2), not (-3) ** 2.
    # Expected: UnaryOp("-", BinOp("**", 3, 2))
    ast = parse(tokenize("-3 ** 2"))
    assert isinstance(ast, UnaryOp) and ast.op == "-"
    assert isinstance(ast.operand, BinOp) and ast.operand.op == "**"


def test_parse_let_and_if():
    ast = parse(tokenize("let x = 5 in if x > 0 then x else -x"))
    # Just check we parsed something non-trivial without raising.
    assert ast is not None


# ── Evaluator (8 tests) ──────────────────────────────────────────


def test_eval_arithmetic_with_precedence():
    assert run("1 + 2 * 3", {}) == 7
    assert run("(1 + 2) * 3", {}) == 9
    assert run("10 - 4 - 3", {}) == 3  # left-assoc
    assert run("10 / 4", {}) == 2  # integer division


def test_eval_power_right_associative():
    # Depends on parser bug #2 being fixed.
    assert run("2 ** 3 ** 2", {}) == 512


def test_eval_unary_minus_with_power():
    # Depends on parser bug #3 being fixed.
    assert run("-3 ** 2", {}) == -9
    assert run("(-3) ** 2", {}) == 9


def test_eval_comparison_operators():
    # Depends on lexer bug #1 being fixed (<= >= == !=).
    assert run("3 <= 3", {}) is True
    assert run("4 <= 3", {}) is False
    assert run("3 >= 3", {}) is True
    assert run("3 == 3", {}) is True
    assert run("3 != 4", {}) is True


def test_eval_and_or_short_circuit():
    # Bug #4: short-circuit. `_witness(0)` raises if called.
    def witness(x):
        raise AssertionError("short-circuit failed — witness was called")

    env = {"w": witness, "z": 0}
    # `z and w(1)`: z is 0 (falsy) → and short-circuits to z (= 0), w not called.
    assert run("z and w(1)", env) == 0
    # `(1 == 1) or w(1)`: lhs is True → or short-circuits, w not called.
    assert run("(1 == 1) or w(1)", env) is True


def test_eval_let_lexical_scope_no_leak():
    # Bug #5: let must be lexical — x must not leak after the
    # let-expression evaluates. Re-running another expression in
    # the same env must not see x.
    env = {}
    val1 = run("let x = 42 in x + 1", env)
    assert val1 == 43
    # x must not have leaked into env.
    assert "x" not in env, f"let leaked binding: env={env}"


def test_eval_let_nested_inner_shadows_outer():
    # Bug #5 also surfaces here: inner let must shadow outer
    # binding for the duration of inner body, then restore.
    assert run("let x = 1 in let x = 2 in x", {}) == 2
    assert run("let x = 1 in (let x = 2 in x) + x", {}) == 3


def test_eval_if_branches():
    assert run("if true then 1 else 2", {}) == 1
    assert run("if false then 1 else 2", {}) == 2
    assert run("if 0 then 1 else 2", {}) == 2  # 0 is falsy
    assert run("if 1 then 1 else 2", {}) == 1


# ── Integration (2 tests) ────────────────────────────────────────


def test_integration_factorial_via_recursive_function():
    # Recursive function passed in env. Tests that calls work and
    # comparison + short-circuit + arithmetic compose correctly.
    def fact(n):
        return 1 if n <= 1 else n * fact(n - 1)

    env = {"fact": fact}
    assert run("fact(5)", env) == 120
    assert run("fact(0)", env) == 1


def test_integration_complex_expression():
    # Exercises lexer + parser + evaluator + env access end-to-end.
    env = {"a": 3, "b": 4, "c": 2}
    # (a ** 2 + b ** 2) >= c ** 4 → (9 + 16) >= 16 → True
    assert run("(a ** 2 + b ** 2) >= c ** 4", env) is True
    # let m = a + b in if m > c then m * c else 0 → 14
    assert run("let m = a + b in if m > c then m * c else 0", env) == 14
