"""Recursive-descent parser for minilang.

Precedence (lowest → highest):
    let
    or
    and
    not
    comparison  (< > <= >= == !=, non-associative, single)
    + -         (left-associative)
    * /         (left-associative)
    unary -     (binds looser than **, so -3 ** 2 == -(3 ** 2))
    **          (right-associative)
    atom

AST nodes are tuples:
    ("num", int)
    ("bool", bool)
    ("var", name)
    ("unop", op, child)          op in {"neg", "not"}
    ("binop", op, left, right)
    ("let", name, value, body)
"""


class Parser:
    def __init__(self, toks):
        self.toks = toks
        self.pos = 0

    def peek(self):
        return self.toks[self.pos]

    def advance(self):
        t = self.toks[self.pos]
        self.pos += 1
        return t

    def _is(self, kind, value):
        t = self.peek()
        return t.kind == kind and t.value == value

    def expect_op(self, op):
        if self._is("OP", op):
            return self.advance()
        raise SyntaxError(f"expected {op!r}, got {self.peek()}")

    def parse(self):
        node = self.expr()
        if self.peek().kind != "EOF":
            raise SyntaxError(f"trailing tokens starting at {self.peek()}")
        return node

    def expr(self):
        if self._is("KW", "let"):
            self.advance()
            name = self.advance()
            if name.kind != "IDENT":
                raise SyntaxError(f"expected identifier after 'let', got {name}")
            self.expect_op("=")
            value = self.expr()
            if not self._is("KW", "in"):
                raise SyntaxError(f"expected 'in', got {self.peek()}")
            self.advance()
            body = self.expr()
            return ("let", name.value, value, body)
        return self.or_expr()

    def or_expr(self):
        node = self.and_expr()
        while self._is("KW", "or"):
            self.advance()
            node = ("binop", "or", node, self.and_expr())
        return node

    def and_expr(self):
        node = self.not_expr()
        while self._is("KW", "and"):
            self.advance()
            node = ("binop", "and", node, self.not_expr())
        return node

    def not_expr(self):
        if self._is("KW", "not"):
            self.advance()
            return ("unop", "not", self.not_expr())
        return self.cmp()

    def cmp(self):
        node = self.add()
        t = self.peek()
        if t.kind == "OP" and t.value in ("<", ">", "<=", ">=", "==", "!="):
            self.advance()
            node = ("binop", t.value, node, self.add())
        return node

    def add(self):
        node = self.mul()
        if self.peek().kind == "OP" and self.peek().value in ("+", "-"):
            op = self.advance().value
            return ("binop", op, node, self.add())
        return node

    def mul(self):
        node = self.unary()
        while self.peek().kind == "OP" and self.peek().value in ("*", "/"):
            op = self.advance().value
            node = ("binop", op, node, self.unary())
        return node

    def unary(self):
        if self._is("OP", "-"):
            self.advance()
            return ("unop", "neg", self.unary())
        return self.power()

    def power(self):
        node = self.atom()
        while self._is("OP", "**"):
            self.advance()
            node = ("binop", "**", node, self.atom())
        return node

    def atom(self):
        t = self.peek()
        if t.kind == "KW" and t.value in ("true", "false"):
            self.advance()
            return ("bool", t.value == "true")
        if t.kind == "NUM":
            self.advance()
            return ("num", t.value)
        if t.kind == "IDENT":
            self.advance()
            return ("var", t.value)
        if self._is("OP", "("):
            self.advance()
            node = self.expr()
            self.expect_op(")")
            return node
        raise SyntaxError(f"unexpected token {t}")


def parse(toks):
    return Parser(toks).parse()
