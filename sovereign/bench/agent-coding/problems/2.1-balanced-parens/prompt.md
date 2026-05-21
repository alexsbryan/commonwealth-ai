# Balanced parentheses (Scaffolded tier)

Decide whether a string of brackets is correctly nested.

## Signature (fixed)

```rust
pub fn is_balanced(s: &str) -> bool
```

## Behaviour

`s` contains ONLY characters from the set `()[]{}`. Return `true` if
every opener has a matching closer of the same type and the brackets
are correctly nested; return `false` otherwise.

Examples:
- `is_balanced("()")` → `true`
- `is_balanced("()[]{}")` → `true`
- `is_balanced("(]")` → `false`
- `is_balanced("([{}])")` → `true`
- `is_balanced("([)]")` → `false` (interleaved, not nested)
- `is_balanced("(")` → `false` (unclosed)
- `is_balanced(")(")` → `false` (close before open)
- `is_balanced("")` → `true` (empty string is vacuously balanced)

## Constraints

- O(n) time, single pass through `s`.
- `s` is guaranteed to contain only the six bracket characters.

## What's in the workdir

```
.
├── Cargo.toml
└── src/
    └── lib.rs   # `is_balanced` stub with `todo!()`
```

## How to deliver

You are running in a tools-driven harness. Mandatory loop:

1. `write` the full file body to `src/lib.rs`.
2. `bash` with command `cargo build 2>&1` — if errors, fix and write again.
3. `bash` with command `cargo test --quiet --test integration` — if any test fails, fix and write again.
4. ONLY after step 3 shows `test result: ok` for all tests, signal completion with the `done` tool.

You MUST NOT signal `done` before `cargo test` has reported all
tests passing. Skipping verification scores zero.

Prefer `write` over `edit`. Files written via tools are the only
thing the grader sees.
