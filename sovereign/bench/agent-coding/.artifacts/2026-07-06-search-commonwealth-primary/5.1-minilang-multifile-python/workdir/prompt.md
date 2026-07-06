# minilang interpreter — cross-file bug fix (Python, multi-file)

The workdir contains a Python package `minilang/`, a small interpreter
for an integer/boolean expression language, split into three stage
files:

```
minilang/
  __init__.py     public API: evaluate(source) -> int | bool
  tokenizer.py    tokenize(src) -> list[Token]
  parser.py       parse(tokens) -> AST   (tuple-based nodes)
  evaluator.py    evaluate_ast(node, env) -> int | bool
```

The package is **structurally complete and imports cleanly**, but it
has **several subtle bugs spread across the three stage files**. Each
bug causes some subset of the held-out integration tests to fail.

The bugs **cascade across files**: a bug in the tokenizer can raise a
`SyntaxError` on an input whose *real* defect lives in the parser or
the evaluator — so a failing test's traceback does **not** reliably
tell you which file to fix. Fix the upstream stage, re-run, and read
the failure as it moves downstream into the next file. Don't assume a
fix in one file generalizes to the next.

## The language (correct semantics)

A single expression. Grammar (informal BNF, lowest → highest
precedence):

```
expr    := 'let' IDENT '=' expr 'in' expr  |  orexpr
orexpr  := andexpr ('or' andexpr)*
andexpr := notexpr ('and' notexpr)*
notexpr := 'not' notexpr  |  cmp
cmp     := add (CMPOP add)?        CMPOP ∈ {< > <= >= == !=}
add     := mul (('+' | '-') mul)*
mul     := unary (('*' | '/') unary)*
unary   := '-' unary  |  power
power   := atom ('**' unary)?      -- right-associative
atom    := INTEGER | IDENT | 'true' | 'false' | '(' expr ')'
```

Semantics that the tests pin:

- **Two-char operators** `<= >= == != **` are single tokens, not two
  single-char tokens.
- **`**` is right-associative**: `2 ** 3 ** 2` ≡ `2 ** 9` = `512`.
- **`+` / `-` are left-associative**: `10 - 3 - 2` = `5`.
- **Unary `-` binds looser than `**`**: `-3 ** 2` ≡ `-(3 ** 2)` = `-9`.
- **`/` is integer floor division**: `7 / 2` = `3` (an int, not `3.5`).
- **`and` / `or` short-circuit**: in `a and b`, if `a` is falsy `b` is
  never evaluated (and vice-versa for `or`). A non-short-circuiting
  impl raises on inputs like `false and (10 / 0 == 0)`, which must
  return `False`.
- **`let x = E1 in E2` is lexically scoped**: the binding is visible
  only inside `E2`; it must not leak into the surrounding environment.
  `let x = 10 in (let x = 1 in 0) + x` = `10`.
- `true` / `false` are boolean literals.
- Unbound name → `NameError`; bad token/parse → `SyntaxError`;
  division by zero → `ZeroDivisionError`.

## How to deliver

The scaffold provides the buggy `minilang/` package plus
`tests/test_integration.py` (a smoke subset). The full held-out suite
grades at exit. All files are small and fully shown in your workdir
view — open the file that owns the stage you're fixing and edit it
there.

- Fix bugs **one file at a time**; re-run the tests after each fix and
  follow the failure as it moves to the next stage.
- `patch_file` / `replace_function` are preferred over rewriting a
  whole file. The pre-write syntax check rejects edits whose result
  isn't valid Python, so broken edits never reach disk.
- When all tests pass, signal completion with `agent_done`.

**Do NOT paste fixed code into chat.** Only files written via tools
count.
