# Config applier — multi-bug fix (Python, attention-tier)

The workdir contains a `config_applier.py` module with four
**independent** utility functions for working with configuration
dictionaries. The module is structurally complete and imports
cleanly, but **each of the four functions has one subtle bug** that
causes a subset of the 12 integration tests to fail.

Your job: read the test failures, identify which bug each one
points at, and fix the four bugs **one at a time**, taking care not
to regress any test that was already passing.

## The four functions

### `deep_merge(base: dict, overlay: dict) -> dict`

Recursively merge `overlay` into `base`. Returns a new dict; does
NOT mutate either input.

- For overlapping keys whose values are both dicts, recurse.
- For any other overlap (including when either side is a list, str,
  int, None, etc.), the overlay value **replaces** the base value
  outright. Lists are not merged or concatenated — they're swapped.
- Keys present in only one input are kept as-is.

### `expand_env(value: str, env: dict) -> str`

Replace every `${KEY}` substring in `value` with `env[KEY]`. The
substitution is **recursive**: if the expansion of `${KEY}`
contains another `${OTHER}`, that one expands too, and so on.

- `${KEY}` where `KEY` is not in `env` → leave the `${KEY}` literal
  unchanged.
- Substitution proceeds left-to-right; expansion of one variable
  may introduce new variables that then expand on a subsequent
  pass. The function returns when no more substitutions apply (a
  fixpoint).
- Cycle detection is NOT required (assume inputs are well-formed).

### `validate_schema(data: dict, schema: dict) -> None`

Validate `data` against `schema`. The schema is a flat dict of
`{key_name: expected_type}` entries where `expected_type` is a
Python type object (e.g., `int`, `str`, `dict`, `list`).

- Every key in `schema` must be present in `data` — missing keys
  raise `ValueError("missing required key: KEY_NAME")`.
- For every key present in both, the data's value must be an
  instance of the schema's expected type. Mismatch raises
  `ValueError("type mismatch at KEY_NAME: expected TYPE, got
  TYPE")`.
- Keys in `data` that are NOT in `schema` are allowed (extra keys
  are fine).
- On success, return `None`.

### `normalize_paths(data: dict, root: str) -> dict`

Walk `data` recursively. For every **string value that looks like a
relative path**, prepend `root + "/"`. Returns a new dict; does NOT
mutate the input.

- A string "looks like a relative path" iff:
  - it does NOT start with `/` (not absolute), AND
  - it contains at least one `/` character OR ends with one of
    the suffixes `.txt`, `.json`, `.yaml`, `.toml`, `.md`, `.cfg`.
- Strings that don't match the above are left as-is — do not
  prepend `root` to arbitrary strings like `"hello"`, `"INFO"`, or
  `"42"`.
- Recurse into nested dicts. Lists are walked but their string
  contents are NOT path-normalized (lists may legitimately contain
  string data unrelated to paths).
- Absolute paths (starting with `/`) are left alone.

## How to deliver

Check the workdir-state preamble above for what files exist. The
scaffold provides `config_applier.py` and `tests/test_integration.py`.

Make targeted fixes — `patch_file` is preferred for single-region
edits because it has less JSON-escape pressure than a full
`write_file` rewrite. The pre-write syntax check rejects any patch
or write whose resulting content is not valid Python; broken edits
never reach disk, so you can re-emit immediately.

When all 12 tests pass, signal completion with `agent_done`.

**Do NOT paste fixed code into chat.** Only files written via tools
count.
