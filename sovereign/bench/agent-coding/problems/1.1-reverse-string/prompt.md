# Reverse a string (Scaffolded tier)

Implement a function that returns the input string with its characters
in reverse order.

## Signature (fixed)

```rust
pub fn reverse_string(s: &str) -> String
```

## Behaviour

- `reverse_string("hello") == "olleh"`
- `reverse_string("") == ""`
- Unicode-correct: multi-byte UTF-8 characters must be reversed as
  whole code points, not bytes (so `reverse_string("héllo")` keeps the
  `é` intact rather than splitting it into two malformed bytes).

You may assume the input is well-formed UTF-8. You do NOT need to
preserve grapheme clusters that span multiple code points — the held
tests check Unicode scalar value order.

## What's in the workdir

```
.
├── Cargo.toml      # already correct; do not modify
└── src/
    └── lib.rs      # contains a `reverse_string` stub with `todo!()`
```

## Constraints

- Standard library only. No `unicode-segmentation` or other crates.
- Single function, public, exact signature as above.
- Do not modify Cargo.toml or the project layout — the grader rebinds
  `reverse_string::reverse_string` exactly as declared.

## How to deliver

You are running in a tools-driven harness. Replace the body of
`reverse_string` in `src/lib.rs` using the **`write` tool** to
rewrite the entire file. Prefer `write` over `edit` — the `edit`
tool requires the `oldText` to match the file byte-for-byte
including whitespace, which is brittle.

The full file to write is short — header doc comments + the
function definition. Reproduce them in your `write` call.

**Files written via tools are the only thing the grader sees.** Pasting
code into chat without calling `write` will score zero.
