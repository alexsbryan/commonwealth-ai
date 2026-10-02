# Group strings by anagram (Scaffolded tier)

Given a vector of strings, partition them into groups of anagrams.

## Signature (fixed)

```rust
pub fn group_anagrams(strs: Vec<String>) -> Vec<Vec<String>>
```

## Behaviour

Two strings are in the same group iff they have the same multiset of
characters (one is a permutation of the other).

**Worked example — memorize this. Most tests are variations.**

```
input:  ["eat", "tea", "tan", "ate", "nat", "bat"]
output: [["eat", "tea", "ate"], ["tan", "nat"], ["bat"]]
```

Three claims that uniquely pin the output structure:

1. `output[0] == ["eat", "tea", "ate"]` — order WITHIN a group is
   the order strings first appeared in input (input indices 0, 1, 3).
2. `output[0]` is first because `input[0]` ("eat") joined this group
   before any other group was started.
3. `output[1]` precedes `output[2]` because `input[2]` ("tan") joined
   its group before `input[5]` ("bat") joined its.

If your code doesn't satisfy all three claims on this example, the
algorithm is wrong. Rethink before writing.

Edge cases:

- empty input → `[]`
- duplicate strings → keep every occurrence in its group
- empty strings → all empty strings cluster into one group

## Constraints

- Inputs are ASCII lowercase letters or the empty string.
- Standard library only. No `IndexMap`, no `itertools`.
- O(N · K log K) time, where N = strings, K = max length.

## Workdir conventions

Check the workdir-state preamble at the top of this message for what
files actually exist. When `tests/integration.rs` is present, it
contains three smoke tests (empty input, single string, canonical
three-group ordering). Passing them is **necessary but not
sufficient** — the grader replaces this file with a private 12-test
suite after you signal `done`. Use `cargo test --quiet --test
integration` to run them.

Crate name is `group_anagrams`; grader rebinds
`group_anagrams::group_anagrams`.

## Execution discipline (load-bearing)

Each `write` REPLACES the whole file. Successive writes without
thinking lose coherence and produce frankenstein code that scores
zero. Follow the stages below in order. Do exactly one stage's
worth of work per turn.

### Stage 1 — PLAN  (no tools)

In your reply, write 2–4 sentences naming the data structures you
will use for (a) the canonical anagram key per string and (b) the
order in which groups first appeared. Reference the worked example
to check yourself. **Do not call any tool in this turn.**

### Stage 2 — WRITE  (one `write`, nothing else)

Emit the complete file body to `src/lib.rs` in a single `write`
call. The code must compile. The code must match your Stage 1 plan.
After the write, stop — do not chain another tool in the same turn.

### Stage 3 — VERIFY  (one `bash`, nothing else)

`bash` with `cargo test --quiet --test integration 2>&1`. This
compiles AND runs the three smoke tests. Stop after the call. Read
the full output before proceeding.

### Stage 4 — FIX  (conditional)

* If `cargo test` shows `test result: ok. 3 passed`: signal `done`.
  Your code passes the smoke suite; the grader will run the full
  private suite next.
* If `cargo test` failed to **compile**: quote the FIRST error line
  verbatim in your reply. Reason about its specific cause in 1–2
  sentences. Return to Stage 2 with a fix targeted at THAT error.
  Do not rewrite untouched parts of the file.
* If `cargo test` **compiled but a test failed**: quote the
  `assertion `left == right`` block. Walk through which of the
  three claims above (1=within-group order, 2=first-appearance
  group, 3=cross-group order) the failure violates. Return to
  Stage 2 with a fix that addresses THAT claim.

## Self-monitor — write counter

Before each `write`, count: this is my Nth write. If N > 3 you are
thrashing. In your reply, summarize what each previous write tried
and why it failed, then commit to one specific change for the next
write. Do not write a fourth time without that summary.

## Tools available

- `read` — read a file in workdir.
- `write` — replace file contents (whole-file).
- `bash` — run shell command in workdir.
- `done` — signal completion. Only after `cargo build` succeeds
  AND you've mentally walked the worked example through your code.
