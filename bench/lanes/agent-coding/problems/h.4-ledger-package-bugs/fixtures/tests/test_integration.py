# SPDX-License-Identifier: AGPL-3.0-or-later
# Held-out integration tests for h.4 — ledger package multi-bug fix.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
import pytest
from ledger import run_ledger, report
from ledger.parsing import parse_line
from ledger.accounts import Accounts


def test_parse_deposit_decimal():
    assert parse_line("ana deposit 12.75") == ("ana", 12.75)


def test_parse_withdraw_negative():
    assert parse_line("bo withdraw 5") == ("bo", -5.0)


def test_parse_skips_blank_and_comments():
    assert parse_line("   ") is None
    assert parse_line("# header") is None


def test_parse_rejects_malformed():
    with pytest.raises(ValueError):
        parse_line("ana deposit")
    with pytest.raises(ValueError):
        parse_line("ana transfer 5")


def test_apply_accumulates():
    a = Accounts()
    a.apply("ana", 10.0)
    a.apply("ana", 2.5)
    assert a.balance("ana") == 12.5


def test_overdraft_rejected():
    a = Accounts()
    a.apply("cy", 10.0)
    with pytest.raises(ValueError, match="overdraft"):
        a.apply("cy", -25.0)


def test_overdraft_leaves_balance_unchanged():
    a = Accounts()
    a.apply("cy", 10.0)
    try:
        a.apply("cy", -25.0)
    except ValueError:
        pass
    assert a.balance("cy") == 10.0


def test_unknown_account_reads_zero():
    assert Accounts().balance("ghost") == 0.0


def test_run_ledger_end_to_end():
    acc = run_ledger([
        "# opening",
        "ana deposit 100.50",
        "bo deposit 20",
        "ana withdraw 0.50",
        "",
    ])
    assert acc.balance("ana") == 100.0
    assert acc.balance("bo") == 20.0


def test_report_orders_by_balance_desc():
    acc = run_ledger(["ana deposit 5", "bo deposit 20", "cy deposit 11"])
    assert report(acc) == ["bo: 20.00", "cy: 11.00", "ana: 5.00"]


def test_report_ties_break_by_name():
    acc = run_ledger(["zed deposit 7", "amy deposit 7"])
    assert report(acc) == ["amy: 7.00", "zed: 7.00"]


def test_report_formats_two_decimals():
    acc = run_ledger(["ana deposit 3.5"])
    assert report(acc) == ["ana: 3.50"]
