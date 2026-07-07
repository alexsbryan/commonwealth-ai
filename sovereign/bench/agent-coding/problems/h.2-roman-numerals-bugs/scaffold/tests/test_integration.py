# SPDX-License-Identifier: AGPL-3.0-or-later
# Smoke tests visible to the agent. A SUBSET of the held-out suite,
# which replaces this file after the agent exits.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
import pytest
from roman import to_roman, from_roman, is_valid, add_roman


def test_smoke_to_roman_nine():
    assert to_roman(9) == "IX"


def test_smoke_from_roman_repeated_glyphs():
    assert from_roman("XX") == 20
    assert from_roman("MMXXVI") == 2026


def test_smoke_is_valid_uppercase():
    assert is_valid("XIV") is True


def test_smoke_add_roman_range():
    with pytest.raises(ValueError, match="sum out of range"):
        add_roman("MMM", "MM")
