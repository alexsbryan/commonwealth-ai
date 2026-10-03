# Reverse a string

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

## Constraints

- Standard library only. No `unicode-segmentation` or other crates.
- Single function, public, exact signature as above.
- Crate name is `reverse_string`. The grader rebinds
  `reverse_string::reverse_string` exactly as declared, so the
  Cargo.toml `[package].name` must be `reverse_string` and the
  function must be public at the crate root.

## How to deliver

You are running in a tools-driven harness. Check the workdir-state
preamble above for what files (if any) already exist. Use the
`write` tool to author whatever is missing (Cargo.toml, src/lib.rs)
and to fill in the function body. Use `bash` with `cargo test
--quiet --test integration 2>&1` to verify, then signal `done`.

Prefer `write` over `edit` — the `edit` tool requires the `oldText`
to match the file byte-for-byte including whitespace, which is
brittle. With `write` you provide the entire file body.

**Files written via tools are the only thing the grader sees.** Pasting
code into chat without calling `write` will score zero.
