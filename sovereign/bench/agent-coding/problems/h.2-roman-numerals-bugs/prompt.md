# Roman numerals — multi-bug fix (Python, attention-tier)

The workdir contains `roman.py`, a structurally complete module with
four functions for Roman numeral arithmetic. **Each of the four
sections has exactly one subtle bug.** Fix all four while keeping
everything else working.

## The contract

- `to_roman(n)` — integer 1..=3999 to canonical numeral
  (`to_roman(9) == "IX"`, `to_roman(1990) == "MCMXC"`).
- `from_roman(s)` — canonical or non-canonical numeral to integer
  using standard subtractive rules (`from_roman("XX") == 20`,
  `from_roman("IX") == 9`). Raises `ValueError` on characters
  outside `IVXLCDM` or empty input.
- `is_valid(s)` — True iff `s` is the CANONICAL uppercase numeral
  for its value (`is_valid("XIV")` is True, `is_valid("IIII")` is
  False).
- `add_roman(a, b)` — the canonical numeral of the sum. When the
  sum exceeds 3999, raises `ValueError` with the message
  `"sum out of range"`.

## Constraints

- Fix bugs surgically — do not rewrite the module's structure.
- Standard library only. The grader imports all four functions from
  `roman` exactly as declared.
