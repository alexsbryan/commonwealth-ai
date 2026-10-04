# SPDX-License-Identifier: AGPL-3.0-or-later
"""Unit conversion: length, mass, temperature, plus quantity parsing
and formatting. Works correctly; the task is structural (see
prompt.md)."""

_LENGTH_TO_METERS = {
    "m": 1.0,
    "km": 1000.0,
    "cm": 0.01,
    "mm": 0.001,
    "mi": 1609.344,
    "ft": 0.3048,
    "in": 0.0254,
}

_MASS_TO_GRAMS = {
    "g": 1.0,
    "kg": 1000.0,
    "mg": 0.001,
    "lb": 453.59237,
    "oz": 28.349523125,
}


def convert_length(value, src, dst):
    if src not in _LENGTH_TO_METERS or dst not in _LENGTH_TO_METERS:
        raise ValueError(f"unknown length unit: {src!r} or {dst!r}")
    meters = value * _LENGTH_TO_METERS[src]
    return meters / _LENGTH_TO_METERS[dst]


def convert_mass(value, src, dst):
    if src not in _MASS_TO_GRAMS or dst not in _MASS_TO_GRAMS:
        raise ValueError(f"unknown mass unit: {src!r} or {dst!r}")
    grams = value * _MASS_TO_GRAMS[src]
    return grams / _MASS_TO_GRAMS[dst]


def convert_temperature(value, src, dst):
    if src == dst:
        return float(value)
    if src == "C":
        celsius = float(value)
    elif src == "F":
        celsius = (value - 32.0) * 5.0 / 9.0
    elif src == "K":
        celsius = value - 273.15
    else:
        raise ValueError(f"unknown temperature unit: {src!r}")
    if dst == "C":
        return celsius
    if dst == "F":
        return celsius * 9.0 / 5.0 + 32.0
    if dst == "K":
        return celsius + 273.15
    raise ValueError(f"unknown temperature unit: {dst!r}")


def parse_quantity(text):
    parts = text.strip().split()
    if len(parts) != 2:
        raise ValueError(f"expected '<number> <unit>': {text!r}")
    raw_value, unit = parts
    try:
        value = float(raw_value)
    except ValueError:
        raise ValueError(f"not a number: {raw_value!r}")
    return value, unit


def convert_quantity(text, dst):
    value, src = parse_quantity(text)
    if src in _LENGTH_TO_METERS:
        result = convert_length(value, src, dst)
    elif src in _MASS_TO_GRAMS:
        result = convert_mass(value, src, dst)
    elif src in ("C", "F", "K"):
        result = convert_temperature(value, src, dst)
    else:
        raise ValueError(f"unknown unit: {src!r}")
    return format_quantity(result, dst)


def format_quantity(value, unit):
    rounded = round(value, 6)
    if rounded == int(rounded):
        return f"{int(rounded)} {unit}"
    return f"{rounded} {unit}"
