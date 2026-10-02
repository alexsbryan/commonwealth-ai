# SPDX-License-Identifier: AGPL-3.0-or-later
"""Account balances. Overdrafts are rejected: a withdrawal that
would take a balance below zero raises ValueError('overdraft')."""


class Accounts:
    def __init__(self):
        self._balances = {}

    def apply(self, name, delta):
        current = self._balances.get(name, 0.0)
        new_balance = current + delta
        self._balances[name] = new_balance
        # BUG: overdraft checked AFTER committing, and the balance
        # is left modified when it raises.
        if new_balance < 0:
            raise ValueError("overdraft")

    def balance(self, name):
        # BUG: unknown accounts should read as 0.0; this raises
        # KeyError instead.
        return self._balances[name]

    def balances(self):
        return dict(self._balances)
