"""Mini expression evaluator — three-stage pipeline (lex → parse → eval).

This module is structurally complete and imports cleanly, but each
of the three stages harbors at least one subtle bug. The 20
integration tests in tests/test_integration.py reveal them.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


# ── Tokens ────────────────────────────────────────────────────────


@dataclass
class Token:
    kind: str
    value: Any
    pos: int


# Single-character tokens that always stand alone.
_SINGLE_CHARS = {
    "+": "PLUS",
    "-": "MINUS",
    "*": "STAR",
    "/": "SLASH",
    "%": "PERCENT",
    "(": "LPAREN",
    ")": "RPAREN",
    ",": "COMMA",
    "=": "ASSIGN",
    "<": "LT",
    ">": "GT",
}

# Keywords (reserved identifiers).
_KEYWORDS = {
    "and": "AND",
    "or": "OR",
    "not": "NOT",
    "if": "IF",
    "then": "THEN",
    "else": "ELSE",
    "let": "LET",
    "in": "IN",
    "true": "TRUE",
    "false": "FALSE",
}


def tokenize(source: str) -> list[Token]:
    """Split source into tokens. Whitespace separates; operators are
    sticky."""
    tokens: list[Token] = []
    i = 0
    n = len(source)
    while i < n:
        c = source[i]
        if c.isspace():
            i += 1
            continue
        if c.isdigit():
            start = i
            while i < n and source[i].isdigit():
                i += 1
            tokens.append(Token("INT", int(source[start:i]), start))
            continue
        if c.isalpha() or c == "_":
            start = i
            while i < n and (source[i].isalnum() or source[i] == "_"):
                i += 1
            word = source[start:i]
            if word in _KEYWORDS:
                tokens.append(Token(_KEYWORDS[word], word, start))
            else:
                tokens.append(Token("IDENT", word, start))
            continue
        if c == "*" and i + 1 < n and source[i + 1] == "*":
            tokens.append(Token("POW", "**", i))
            i += 2
            continue
        # BUG #1: comparison operators `<=`, `>=`, `==`, `!=` are
        # lexed character-by-character below; they should be lexed
        # as single two-character tokens BEFORE the single-char
        # fall-through. Tests for comparison operators will fail.
        if c in _SINGLE_CHARS:
            tokens.append(Token(_SINGLE_CHARS[c], c, i))
            i += 1
            continue
        if c == "!":
            if i + 1 < n and source[i + 1] == "=":
                tokens.append(Token("NEQ", "!=", i))
                i += 2
                continue
            raise SyntaxError(f"unexpected '!' at position {i}")
        raise SyntaxError(f"unrecognized character '{c}' at position {i}")
    return tokens


# ── AST nodes ─────────────────────────────────────────────────────


@dataclass
class IntLit:
    value: int


@dataclass
class BoolLit:
    value: bool


@dataclass
class VarRef:
    name: str


@dataclass
class UnaryOp:
    op: str
    operand: Any


@dataclass
class BinOp:
    op: str
    left: Any
    right: Any


@dataclass
class IfExpr:
    cond: Any
    then_branch: Any
    else_branch: Any


@dataclass
class LetExpr:
    name: str
    value: Any
    body: Any


@dataclass
class Call:
    name: str
    args: list


# ── Parser (recursive descent) ────────────────────────────────────


class Parser:
    def __init__(self, tokens: list[Token]):
        self.tokens = tokens
        self.i = 0

    def peek(self) -> Token | None:
        return self.tokens[self.i] if self.i < len(self.tokens) else None

    def consume(self) -> Token:
        t = self.tokens[self.i]
        self.i += 1
        return t

    def expect(self, kind: str) -> Token:
        t = self.peek()
        if t is None or t.kind != kind:
            raise SyntaxError(f"expected {kind}, got {t}")
        return self.consume()

    def parse_expr(self) -> Any:
        return self.parse_or()

    def parse_or(self) -> Any:
        node = self.parse_and()
        while self.peek() and self.peek().kind == "OR":
            self.consume()
            right = self.parse_and()
            node = BinOp("or", node, right)
        return node

    def parse_and(self) -> Any:
        node = self.parse_not()
        while self.peek() and self.peek().kind == "AND":
            self.consume()
            right = self.parse_not()
            node = BinOp("and", node, right)
        return node

    def parse_not(self) -> Any:
        if self.peek() and self.peek().kind == "NOT":
            self.consume()
            return UnaryOp("not", self.parse_not())
        return self.parse_cmp()

    def parse_cmp(self) -> Any:
        node = self.parse_add()
        t = self.peek()
        if t and t.kind in ("LT", "GT", "LE", "GE", "EQ", "NEQ"):
            op = self.consume().value
            right = self.parse_add()
            node = BinOp(op, node, right)
        return node

    def parse_add(self) -> Any:
        node = self.parse_mul()
        while self.peek() and self.peek().kind in ("PLUS", "MINUS"):
            op = self.consume().value
            right = self.parse_mul()
            node = BinOp(op, node, right)
        return node

    def parse_mul(self) -> Any:
        node = self.parse_pow()
        while self.peek() and self.peek().kind in ("STAR", "SLASH", "PERCENT"):
            op = self.consume().value
            right = self.parse_pow()
            node = BinOp(op, node, right)
        return node

    def parse_pow(self) -> Any:
        node = self.parse_unary()
        if self.peek() and self.peek().kind == "POW":
            self.consume()
            # BUG #2: spec says ** is right-associative; this
            # parses left-associative by recursing into parse_unary
            # rather than parse_pow.
            right = self.parse_unary()
            node = BinOp("**", node, right)
        return node

    def parse_unary(self) -> Any:
        t = self.peek()
        if t and t.kind in ("MINUS", "PLUS"):
            op = self.consume().value
            # BUG #3: spec says unary - has LOWER precedence than
            # **, so `-3 ** 2` parses as -(3 ** 2) = -9. Recursing
            # into parse_atom here binds unary too tightly — the
            # `-` consumes only `3`, yielding (-3) ** 2 = 9.
            operand = self.parse_atom()
            return UnaryOp(op, operand)
        return self.parse_atom()

    def parse_atom(self) -> Any:
        t = self.peek()
        if t is None:
            raise SyntaxError("unexpected end of input")
        if t.kind == "INT":
            self.consume()
            return IntLit(t.value)
        if t.kind == "TRUE":
            self.consume()
            return BoolLit(True)
        if t.kind == "FALSE":
            self.consume()
            return BoolLit(False)
        if t.kind == "LPAREN":
            self.consume()
            inner = self.parse_expr()
            self.expect("RPAREN")
            return inner
        if t.kind == "IF":
            self.consume()
            cond = self.parse_expr()
            self.expect("THEN")
            then_branch = self.parse_expr()
            self.expect("ELSE")
            else_branch = self.parse_expr()
            return IfExpr(cond, then_branch, else_branch)
        if t.kind == "LET":
            self.consume()
            name_tok = self.expect("IDENT")
            self.expect("ASSIGN")
            value = self.parse_expr()
            self.expect("IN")
            body = self.parse_expr()
            return LetExpr(name_tok.value, value, body)
        if t.kind == "IDENT":
            self.consume()
            if self.peek() and self.peek().kind == "LPAREN":
                self.consume()
                args = []
                if not (self.peek() and self.peek().kind == "RPAREN"):
                    args.append(self.parse_expr())
                    while self.peek() and self.peek().kind == "COMMA":
                        self.consume()
                        args.append(self.parse_expr())
                self.expect("RPAREN")
                return Call(t.value, args)
            return VarRef(t.value)
        raise SyntaxError(f"unexpected token {t}")


def parse(tokens: list[Token]) -> Any:
    return Parser(tokens).parse_expr()


# ── Evaluator ─────────────────────────────────────────────────────


def evaluate(node: Any, env: dict) -> Any:
    if isinstance(node, IntLit):
        return node.value
    if isinstance(node, BoolLit):
        return node.value
    if isinstance(node, VarRef):
        if node.name not in env:
            raise NameError(f"unbound name: {node.name}")
        return env[node.name]
    if isinstance(node, UnaryOp):
        v = evaluate(node.operand, env)
        if node.op == "-":
            return -v
        if node.op == "+":
            return +v
        if node.op == "not":
            return not v
    if isinstance(node, BinOp):
        # BUG #4: spec says `and`/`or` short-circuit. Evaluating
        # both operands up front (`l` and `r` below) breaks the
        # short-circuit guarantee — _witness(x) tests will fail.
        l = evaluate(node.left, env)
        r = evaluate(node.right, env)
        op = node.op
        if op == "+": return l + r
        if op == "-": return l - r
        if op == "*": return l * r
        if op == "/": return l // r if isinstance(l, int) else l / r
        if op == "%": return l % r
        if op == "**": return l ** r
        if op == "==": return l == r
        if op == "!=": return l != r
        if op == "<": return l < r
        if op == "<=": return l <= r
        if op == ">": return l > r
        if op == ">=": return l >= r
        if op == "and": return l and r
        if op == "or": return l or r
        raise SyntaxError(f"unknown op: {op}")
    if isinstance(node, IfExpr):
        if evaluate(node.cond, env):
            return evaluate(node.then_branch, env)
        return evaluate(node.else_branch, env)
    if isinstance(node, LetExpr):
        # BUG #5: spec says let bindings are lexical — the binding
        # should be restored after `body` evaluates. Mutating env
        # without restoration leaks `x` into the caller's env (and
        # into sibling let-expressions sharing the same env dict).
        env[node.name] = evaluate(node.value, env)
        return evaluate(node.body, env)
    if isinstance(node, Call):
        if node.name not in env:
            raise NameError(f"unbound name: {node.name}")
        fn = env[node.name]
        args = [evaluate(a, env) for a in node.args]
        return fn(*args)
    raise SyntaxError(f"unknown AST node: {type(node).__name__}")


# ── Top-level ─────────────────────────────────────────────────────


def run(source: str, env: dict) -> Any:
    """Top-level: tokenize → parse → evaluate. The grader binds
    here."""
    tokens = tokenize(source)
    ast = parse(tokens)
    return evaluate(ast, env)
