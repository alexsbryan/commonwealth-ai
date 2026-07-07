# SPDX-License-Identifier: AGPL-3.0-or-later
# Smoke tests visible to the agent — a SUBSET of the held-out suite,
# which replaces this file after the agent exits.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
import pytest
from ledger import run_ledger, report
from ledger.parsing import parse_line
from ledger.accounts import Accounts


def test_smoke_parse_decimal_amounts():
    assert parse_line("ana deposit 12.75") == ("ana", 12.75)


def test_smoke_withdraw_is_negative():
    assert parse_line("bo withdraw 5") == ("bo", -5.0)


def test_smoke_overdraft_rejected_without_commit():
    a = Accounts()
    a.apply("cy", 10.0)
    with pytest.raises(ValueError, match="overdraft"):
        a.apply("cy", -25.0)
    assert a.balance("cy") == 10.0


def test_smoke_unknown_account_reads_zero():
    assert Accounts().balance("nobody") == 0.0


def test_smoke_report_orders_by_balance_desc():
    acc = run_ledger(["ana deposit 5", "bo deposit 20"])
    assert report(acc) == ["bo: 20.00", "ana: 5.00"]
