# SPDX-License-Identifier: AGPL-3.0-or-later
"""One transaction per line: '<name> deposit|withdraw <amount>'.
Blank lines and lines starting with '#' are ignored."""


def parse_line(line):
    text = line.strip()
    if not text or text.startswith("#"):
        return None
    parts = text.split()
    if len(parts) != 3:
        raise ValueError(f"malformed line: {line!r}")
    name, op, raw_amount = parts
    # BUG: int() truncates cents; amounts are decimal ("12.75").
    amount = int(float(raw_amount))
    if op == "deposit":
        return name, amount
    if op == "withdraw":
        # BUG: withdrawals must be negative; this returns positive.
        return name, amount
    raise ValueError(f"unknown op: {op!r}")
