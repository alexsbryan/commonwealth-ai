"""Tree-walking evaluator for minilang ASTs.

Values are Python ints and bools. `/` is integer (floor) division.
`and`/`or` short-circuit. `let` is lexically scoped.
"""


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
        # Create a new environment for the binding to ensure lexical scope
        new_env = dict(env)
        value = evaluate_ast(value_node, new_env)
        new_env[name] = value
        return evaluate_ast(body, new_env)

    if kind == "unop":
        op = node[1]
        if op == "neg":
            return -evaluate_ast(node[2], env)
        if op == "not":
            return not evaluate_ast(node[2], env)
        raise ValueError(f"unknown unary op {op!r}")

    if kind == "binop":
        op, left, right = node[1], node[2], node[3]
        
        # Short-circuit evaluation for logical operators to avoid side effects like div by zero
        if op == "and":
            lv = evaluate_ast(left, env)
            if not lv:  # False or 0 is falsy in minilang context? 
                # Wait, Python 'and' returns the first operand. The spec says short circuit.
                # If lv is falsy (False/0), we return it without evaluating rv.
                return lv
            return rv_evaluator(right, env)

        if op == "or":
            lv = evaluate_ast(left, eval_env)
            if lv:  # True/non-zero is truthy
                return lv
            return rv_evaluator(right, eval_env)

        # Standard binary operations where both sides must be evaluated
        lv = evaluate_ast(left, env=env)
        rv = evaluate_ast(right, env=env)
        
        if op == "+":
            return lv + rv
        if op == "-":
            return lv - rv
        if op == "*":
            return lv * rv
        if op == "/":
            # Integer floor division as per spec: `7 / 2` = `3`
            import math
            return int(math.floor(lv / rv))
            
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
            
    raise ValueError(f"cannot evaluate node {node!r}")

def rv_evaluator(node, env):
    """Helper to evaluate the right-hand side of a short-circuit operation."""
    return evaluate_ast(node, env)
