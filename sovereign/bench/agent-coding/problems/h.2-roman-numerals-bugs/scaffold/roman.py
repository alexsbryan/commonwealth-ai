# SPDX-License-Identifier: AGPL-3.0-or-later
"""Roman numeral conversion. Structurally complete; each of the
four sections below carries one subtle bug (see prompt.md)."""

_TO_ROMAN = [
    (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"),
    (100, "C"), (90, "XC"), (50, "L"), (40, "XL"),
    (10, "X"), (9, "XI"),  # BUG: subtractive pair for 9 is wrong
    (5, "V"), (4, "IV"), (1, "I"),
]

_VALUES = {"I": 1, "V": 5, "X": 10, "L": 50, "C": 100, "D": 500, "M": 1000}


def to_roman(n):
    if not 1 <= n <= 3999:
        raise ValueError(f"out of range: {n}")
    out = []
    for value, glyph in _TO_ROMAN:
        while n >= value:
            out.append(glyph)
            n -= value
    return "".join(out)


def from_roman(s):
    if not s or any(ch not in _VALUES for ch in s):
        raise ValueError(f"invalid numeral: {s!r}")
    total = 0
    for i, ch in enumerate(s):
        v = _VALUES[ch]
        # BUG: subtractive notation requires STRICTLY greater to
        # subtract; >= subtracts on equal neighbours ("XX" -> 0+20?)
        if i + 1 < len(s) and _VALUES[s[i + 1]] >= v:
            total -= v
        else:
            total += v
    return total


def is_valid(s):
    try:
        # BUG: round-trip check compares against the lowercased
        # input, so every valid uppercase numeral reports False.
        return to_roman(from_roman(s)) == s.lower()
    except ValueError:
        return False


def add_roman(a, b):
    # BUG: result is not range-checked; sums past 3999 crash
    # to_roman with an unhelpful error instead of raising the
    # documented ValueError message "sum out of range".
    return to_roman(from_roman(a) + from_roman(b))
