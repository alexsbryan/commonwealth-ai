# SPDX-License-Identifier: AGPL-3.0-or-later
# Held-out behavior tests for h.3 — units split.
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
