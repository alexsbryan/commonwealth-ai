# SPDX-License-Identifier: AGPL-3.0-or-later
# Smoke tests visible to the agent during development. These are a
# SUBSET of the held-out grading suite, which replaces this file
# (same path, same name) AFTER the agent exits. Passing all three is
# necessary but not sufficient.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from rle import encode, decode


def test_smoke_encode_basic():
    assert encode("AABBBCCCC") == "2A3B4C"


def test_smoke_singles_have_no_count():
    assert encode("XYZ") == "XYZ"
    assert decode("XYZ") == "XYZ"


def test_smoke_round_trip():
    s = "WWWWWWWWWWWWBWWWWWWWWWWWWBBBWWWWWWWWWWWWWWWWWWWWWWWWB"
    assert decode(encode(s)) == s
