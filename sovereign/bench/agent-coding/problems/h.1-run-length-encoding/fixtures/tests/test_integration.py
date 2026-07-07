# SPDX-License-Identifier: AGPL-3.0-or-later
# Held-out integration tests for h.1 — run-length encoding.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from rle import encode, decode


def test_encode_empty():
    assert encode("") == ""


def test_decode_empty():
    assert decode("") == ""


def test_encode_basic_runs():
    assert encode("AABBBCCCC") == "2A3B4C"


def test_encode_single_characters_uncounted():
    assert encode("XYZ") == "XYZ"


def test_encode_mixed_runs_and_singles():
    assert encode("WWWWWWWWWWWWBWWWWWWWWWWWWBBBWWWWWWWWWWWWWWWWWWWWWWWWB") == "12WB12W3B24WB"


def test_encode_lowercase_and_spaces():
    # Spaces are data like any other character (Exercism canonical case).
    assert encode("  hsqq qww  ") == "2 hs2q q2w2 "


def test_decode_basic():
    assert decode("2A3B4C") == "AABBBCCCC"


def test_decode_singles():
    assert decode("XYZ") == "XYZ"


def test_decode_mixed():
    assert decode("12WB12W3B24WB") == "WWWWWWWWWWWWBWWWWWWWWWWWWBBBWWWWWWWWWWWWWWWWWWWWWWWWB"


def test_decode_multi_digit_counts():
    assert decode("10A1B") == "A" * 10 + "B"


def test_round_trip_long():
    s = "zzz zz z zz" * 7
    assert decode(encode(s)) == s


def test_round_trip_unicode_scalars():
    s = "ééé--üü"
    assert decode(encode(s)) == s
