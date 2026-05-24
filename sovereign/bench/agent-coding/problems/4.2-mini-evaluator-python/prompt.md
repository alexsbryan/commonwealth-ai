# Mini expression evaluator — cascading bug fix (Python, attention-tier)

The workdir contains `evaluator.py`, a small interpreter for an
arithmetic / boolean expression language. The interpreter has FIVE
subtle bugs distributed across its three stages — tokenizer,
parser, evaluator. Each bug causes some subset of the 20
integration tests to fail.

The bugs **cascade**: a fix in an earlier stage will cause
previously-failing tests (which died early at that stage) to now
run further and reveal bugs in later stages. Read each test
failure on its own terms; don't assume a fix-pattern from one cycle
generalizes to the next.

The file is ~260 lines, above the inline-anchor cap. Your
position-0 view shows function signatures with line numbers; you'll
need to use those as navigation anchors and use `patch_file` with
line ranges you infer from the outline.

## The language

A single expression. Tokens are separated by whitespace OR by
operator boundaries. The grammar (in informal BNF):

```
expr    := orexpr
orexpr  := andexpr ('or' andexpr)*
andexpr := notexpr ('and' notexpr)*
notexpr := 'not' notexpr  |  cmpexpr
cmpexpr := addexpr (CMPOP addexpr)?
           where CMPOP ∈ {==, !=, <, <=, >, >=}
addexpr := mulexpr (('+' | '-') mulexpr)*
mulexpr := powexpr (('*' | '/' | '%') powexpr)*
powexpr := unary ('**' powexpr)?    -- right-associative
unary   := ('-' | '+') unary  |  atom
atom    := INTEGER
         | IDENT
         | IDENT '(' [expr (',' expr)*] ')'
         | '(' expr ')'
         | 'if' expr 'then' expr 'else' expr
         | 'let' IDENT '=' expr 'in' expr
```

Notes:
- `**` is **right-associative**: `2 ** 3 ** 2` ≡ `2 ** 9` = `512`,
  not `(2 ** 3) ** 2` = `64`.
- Unary `-` has **lower** precedence than `**`: `-3 ** 2` ≡
  `-(3 ** 2)` = `-9`, not `(-3) ** 2` = `9`. This matches Python
  and mathematical convention.
- `and` / `or` **short-circuit**: in `a or b`, if `a` is truthy
  `b` is never evaluated; in `a and b`, if `a` is falsy `b` is
  never evaluated. The tests will use a function `_witness(x)`
  bound in the environment that records when it's called — a
  non-short-circuiting impl will fail these tests.
- `let x = E1 in E2` is **lexical**: `E2` is evaluated in an
  environment where `x` is bound to the value of `E1`. The outer
  binding is RESTORED after `E2` evaluates. A dynamic-scoping impl
  would leak `x` into subsequent expressions in the same env;
  tests assert no such leak.
- Comparison operators are **non-associative** and **chain like
  Python** is NOT supported: `1 < 2 < 3` is a parse error (only
  one comparison per cmpexpr).

## The function the grader binds

```python
def run(source: str, env: dict) -> int | bool
```

Parses and evaluates `source` against `env`. `env` is a `dict[str,
int | bool | callable]`. Returns the resulting value.

Errors:
- Unrecognized character → `SyntaxError`
- Parse failure → `SyntaxError`
- Reference to unbound name → `NameError`
- Division by zero → `ZeroDivisionError`

## How to deliver

The scaffold provides `evaluator.py` (~260 lines, three-stage
pipeline) and `tests/test_integration.py` (20 tests organized by
stage). Both are visible in the workdir state at position 0.

The file is above the inline-anchor cap, so position 0 shows
function signatures with line numbers only. Use the outline to
locate the function, then `patch_file` with a line range you
infer. If your inferred range is wrong, the executor returns the
actual file length in the rejection — re-emit with corrected
range.

`patch_file` is preferred over `write_file` for surgical fixes.
The pre-write syntax check rejects patches whose post-patch
content isn't valid Python; broken edits never reach disk.

When all 20 tests pass, signal completion with `agent_done`.

**Do NOT paste fixed code into chat.** Only files written via
tools count.
