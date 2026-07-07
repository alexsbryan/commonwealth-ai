# SPDX-License-Identifier: AGPL-3.0-or-later
# Behavior tests + the structural ladder. The grader's held-out
# suite replaces this file (behavior only) after the agent exits.
import sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
from units import (
    convert_length, convert_mass, convert_temperature,
    parse_quantity, convert_quantity, format_quantity,
)


def test_length():
    assert abs(convert_length(1, "km", "m") - 1000.0) < 1e-9


def test_mass():
    assert abs(convert_mass(2, "kg", "lb") - 4.409245243697551) < 1e-9


def test_temperature():
    assert convert_temperature(212, "F", "C") == 100.0


def test_parse():
    assert parse_quantity(" 3.5 km ") == (3.5, "km")


def test_convert_quantity_end_to_end():
    assert convert_quantity("1 km", "m") == "1000 m"


def test_format():
    assert format_quantity(2.5, "kg") == "2.5 kg"


def _max_source_lines():
    root = os.path.dirname(os.path.dirname(__file__))
    worst = 0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in ("tests", "__pycache__", ".git")]
        for name in filenames:
            if name.endswith(".py"):
                with open(os.path.join(dirpath, name)) as fh:
                    worst = max(worst, sum(1 for _ in fh))
    return worst


# Structural ladder — the task's goal from the prompt ("every source
# file <= 30 lines"), one rung per threshold so each extraction step
# is a strict improvement.

def test_largest_source_file_within_80_lines():
    assert _max_source_lines() <= 80


def test_largest_source_file_within_60_lines():
    assert _max_source_lines() <= 60


def test_largest_source_file_within_45_lines():
    assert _max_source_lines() <= 45


def test_every_source_file_within_30_lines():
    assert _max_source_lines() <= 30
