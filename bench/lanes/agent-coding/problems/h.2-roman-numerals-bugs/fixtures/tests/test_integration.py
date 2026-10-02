# SPDX-License-Identifier: AGPL-3.0-or-later
# Held-out integration tests for h.2 — roman numeral multi-bug fix.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
import pytest
from roman import to_roman, from_roman, is_valid, add_roman


def test_to_roman_nine():
    assert to_roman(9) == "IX"


def test_to_roman_nineteen_ninety():
    assert to_roman(1990) == "MCMXC"


def test_to_roman_full_range_spot():
    assert to_roman(3999) == "MMMCMXCIX"
    assert to_roman(49) == "XLIX"


def test_from_roman_repeated():
    assert from_roman("XX") == 20
    assert from_roman("III") == 3


def test_from_roman_subtractive():
    assert from_roman("IX") == 9
    assert from_roman("MCMXC") == 1990


def test_from_roman_year():
    assert from_roman("MMXXVI") == 2026


def test_from_roman_rejects_garbage():
    with pytest.raises(ValueError):
        from_roman("IQ")
    with pytest.raises(ValueError):
        from_roman("")


def test_is_valid_true_cases():
    assert is_valid("XIV") is True
    assert is_valid("MMMCMXCIX") is True


def test_is_valid_false_cases():
    assert is_valid("IIII") is False
    assert is_valid("VX") is False


def test_add_roman_basic():
    assert add_roman("XIV", "VI") == "XX"


def test_add_roman_carries():
    assert add_roman("MCMXC", "XXXVI") == "MMXXVI"


def test_add_roman_sum_out_of_range():
    with pytest.raises(ValueError, match="sum out of range"):
        add_roman("MMM", "MM")
