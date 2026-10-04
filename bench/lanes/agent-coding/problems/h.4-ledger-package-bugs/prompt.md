# Ledger — multi-file package, multi-bug fix (Python)

The workdir contains a `ledger/` package with three modules:
`parsing.py` (transaction lines), `accounts.py` (balances with
overdraft protection), and `__init__.py` (end-to-end run + report).
The package imports cleanly, but it carries **five bugs spread
across the three files**. Fix all five while keeping working
behavior intact.

## The contract

- `parse_line("<name> deposit|withdraw <amount>")` → `(name, signed
  float amount)`; withdrawals are NEGATIVE; decimal amounts keep
  their cents; blank lines and `#` comments → `None`; malformed
  lines and unknown ops raise `ValueError`.
- `Accounts.apply(name, delta)` — rejects any withdrawal that would
  take a balance below zero with `ValueError("overdraft")`, and a
  rejected withdrawal must leave the balance UNCHANGED.
- `Accounts.balance(name)` — unknown accounts read as `0.0`.
- `report(accounts)` — rows formatted `"<name>: <balance with two
  decimals>"`, ordered by balance DESCENDING, ties broken by name
  ascending.

## Constraints

- Fix bugs surgically; keep the three-module structure.
- Standard library only. The grader imports exactly as the tests do.
