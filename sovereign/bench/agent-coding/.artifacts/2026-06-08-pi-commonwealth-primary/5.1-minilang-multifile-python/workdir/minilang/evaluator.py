"""Tree-walking evaluator for minilang ASTs.

Values are Python ints and bools. `/` is integer (floor) division.
`and`/`or` short-circuit. `let` is lexically scoped.
"""


def evaluate_ast(node, env):
    kind = node[0]

    if kind == "num":
        return node[1]

    if kind == "bool":
        return node[1]

    if kind == "var":
        name = node[1]
        if name not in env:
            raise NameError(f"unbound variable {name!r}")
        return env[name]

    if kind == "let":
        _, name, value_node, body = node
        value = evaluate_ast(value_node, env)
        env[name] = value
        return evaluate_ast(body, env)

    if kind == "unop":
        op = node[1]
        if op == "neg":
            return -evaluate_ast(node[2], env)
        if op == "not":
            return not evaluate_ast(node[2], env)
        raise ValueError(f"unknown unary op {op!r}")

    if kind == "binop":
        op, left, right = node[1], node[2], node[3]
        lv = evaluate_ast(left, env)
        rv = evaluate_ast(right, env)
        if op == "and":
            return lv and rv
        if op == "or":
            return lv or rv
        if op == "+":
            return lv + rv
        if op == "-":
            return lv - rv
        if op == "*":
            return lv * rv
        if op == "/":
            return lv / rv
        if op == "**":
            return lv ** rv
        if op == "<":
            return lv < rv
        if op == ">":
            return lv > rv
        if op == "<=":
            return lv <= rv
        if op == ">=":
            return lv >= rv
        if op == "==":
            return lv == rv
        if op == "!=":
            return lv != rv
        raise ValueError(f"unknown binary op {op!r}")

    raise ValueError(f"cannot evaluate node {node!r}")
