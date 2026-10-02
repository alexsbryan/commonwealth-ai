"""Calculator module — public API: evaluate, solve_linear, statistics."""


def evaluate(expression):
    """Evaluate a simple arithmetic expression with +, -, *, /, parens."""
    tokens = _tokenize(expression)
    rpn = _to_rpn(tokens)
    return _eval_rpn(rpn)


def solve_linear(a, b):
    """Solve a*x + b == 0 for x. Returns None if a == 0 and b != 0."""
    if a == 0:
        return None if b != 0 else 0
    return -b / a


def statistics(values):
    """Return (mean, variance, stddev) for a list of numbers."""
    if not values:
        return (0.0, 0.0, 0.0)
    mean = sum(values) / len(values)
    var = sum((v - mean) ** 2 for v in values) / len(values)
    return (mean, var, var ** 0.5)


def _tokenize(expression):
    """Lex an arithmetic expression into a list of tokens."""
    tokens = []
    i = 0
    n = len(expression)
    while i < n:
        ch = expression[i]
        if ch.isspace():
            i += 1
            continue
        if ch in "+-*/()":
            tokens.append(ch)
            i += 1
            continue
        if ch.isdigit() or ch == ".":
            j = i
            while j < n and (expression[j].isdigit() or expression[j] == "."):
                j += 1
            tokens.append(float(expression[i:j]))
            i = j
            continue
        raise ValueError(f"unexpected char at {i}: {ch!r}")
    return tokens


def _to_rpn(tokens):
    """Shunting-yard: convert infix tokens to reverse Polish notation."""
    out = []
    ops = []
    prec = {"+": 1, "-": 1, "*": 2, "/": 2}
    for tok in tokens:
        if isinstance(tok, float):
            out.append(tok)
        elif tok in prec:
            while ops and ops[-1] != "(" and prec.get(ops[-1], 0) >= prec[tok]:
                out.append(ops.pop())
            ops.append(tok)
        elif tok == "(":
            ops.append(tok)
        elif tok == ")":
            while ops and ops[-1] != "(":
                out.append(ops.pop())
            if not ops:
                raise ValueError("mismatched paren")
            ops.pop()
    while ops:
        op = ops.pop()
        if op == "(":
            raise ValueError("mismatched paren")
        out.append(op)
    return out


def _eval_rpn(rpn):
    """Evaluate tokens in reverse Polish notation."""
    stack = []
    for tok in rpn:
        if isinstance(tok, float):
            stack.append(tok)
        else:
            b = stack.pop()
            a = stack.pop()
            if tok == "+":
                stack.append(a + b)
            elif tok == "-":
                stack.append(a - b)
            elif tok == "*":
                stack.append(a * b)
            elif tok == "/":
                stack.append(a / b)
    return stack[0]
