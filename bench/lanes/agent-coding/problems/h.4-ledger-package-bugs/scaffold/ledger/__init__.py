# SPDX-License-Identifier: AGPL-3.0-or-later
from .parsing import parse_line
from .accounts import Accounts


def run_ledger(lines):
    accounts = Accounts()
    for line in lines:
        entry = parse_line(line)
        if entry is not None:
            accounts.apply(*entry)
    return accounts


def report(accounts):
    # Documented order: by balance DESCENDING, ties by name ascending.
    rows = accounts.balances()
    # BUG: sorts by name only, ignoring the documented balance order.
    ordered = sorted(rows.items())
    return [f"{name}: {balance:.2f}" for name, balance in ordered]
