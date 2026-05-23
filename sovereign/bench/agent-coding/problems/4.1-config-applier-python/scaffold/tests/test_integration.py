"""Held-out integration tests for 4.1 config-applier (Python).

Copied into the agent's workdir by the witness pipeline AFTER the
agent exits. Anything the agent wrote under `tests/` is overwritten;
the held-out cases below are canonical.

12 tests, 3 per function. Each test name encodes the function under
test so the model can map failures back to a buggy function from the
pytest output alone.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from config_applier import (  # noqa: E402
    deep_merge,
    expand_env,
    normalize_paths,
    validate_schema,
)


# ── deep_merge ────────────────────────────────────────────────────


def test_deep_merge_flat_dicts():
    base = {"a": 1, "b": 2}
    overlay = {"b": 20, "c": 30}
    assert deep_merge(base, overlay) == {"a": 1, "b": 20, "c": 30}


def test_deep_merge_nested_dicts_recurse():
    base = {"db": {"host": "localhost", "port": 5432, "pool": {"size": 5}}}
    overlay = {"db": {"port": 5433, "pool": {"size": 10, "timeout": 30}}}
    result = deep_merge(base, overlay)
    assert result == {
        "db": {
            "host": "localhost",
            "port": 5433,
            "pool": {"size": 10, "timeout": 30},
        }
    }


def test_deep_merge_lists_are_replaced_not_concatenated():
    # Per the spec: list values REPLACE, they don't concatenate.
    base = {"plugins": ["alpha", "beta"]}
    overlay = {"plugins": ["gamma"]}
    assert deep_merge(base, overlay) == {"plugins": ["gamma"]}


# ── expand_env ────────────────────────────────────────────────────


def test_expand_env_single_substitution():
    assert expand_env("hello ${NAME}", {"NAME": "world"}) == "hello world"


def test_expand_env_multiple_substitutions_in_one_string():
    out = expand_env(
        "${GREET}, ${NAME}! Welcome to ${PLACE}.",
        {"GREET": "Hi", "NAME": "Ada", "PLACE": "Earth"},
    )
    assert out == "Hi, Ada! Welcome to Earth."


def test_expand_env_nested_substitution_is_recursive():
    # ${A} expands to "x${B}y"; that introduces ${B} which must
    # expand on the next pass. Final result: "xZy".
    env = {"A": "x${B}y", "B": "Z"}
    assert expand_env("${A}", env) == "xZy"


# ── validate_schema ───────────────────────────────────────────────


def test_validate_schema_passes_valid_data():
    schema = {"name": str, "age": int}
    data = {"name": "Ada", "age": 30}
    # Should not raise.
    assert validate_schema(data, schema) is None


def test_validate_schema_raises_on_type_mismatch():
    schema = {"name": str, "age": int}
    data = {"name": "Ada", "age": "thirty"}
    with pytest.raises(ValueError) as exc:
        validate_schema(data, schema)
    assert "age" in str(exc.value)


def test_validate_schema_raises_on_missing_required_key():
    # Per the spec: every key in the schema must be present in data.
    schema = {"name": str, "age": int}
    data = {"name": "Ada"}  # age missing
    with pytest.raises(ValueError) as exc:
        validate_schema(data, schema)
    assert "age" in str(exc.value)


# ── normalize_paths ───────────────────────────────────────────────


def test_normalize_paths_prepends_root_for_relative_paths():
    data = {
        "config_file": "etc/app.toml",
        "log_dir": "var/log",
    }
    result = normalize_paths(data, "/srv/app")
    assert result == {
        "config_file": "/srv/app/etc/app.toml",
        "log_dir": "/srv/app/var/log",
    }


def test_normalize_paths_leaves_absolute_paths_and_non_paths_alone():
    # Absolute paths start with /; non-path strings ("INFO", "42",
    # "hello") should not be prepended.
    data = {
        "absolute": "/etc/passwd",
        "level": "INFO",
        "count_label": "42",
        "title": "hello world",
        "config": "settings.toml",  # path-shaped → prepend
    }
    result = normalize_paths(data, "/srv/app")
    assert result == {
        "absolute": "/etc/passwd",
        "level": "INFO",
        "count_label": "42",
        "title": "hello world",
        "config": "/srv/app/settings.toml",
    }


def test_normalize_paths_recurses_into_nested_dicts():
    data = {
        "outer_log": "logs/out.log",
        "nested": {
            "inner_cfg": "cfg/inner.toml",
            "label": "MYLABEL",  # not path-shaped → leave alone
        },
    }
    result = normalize_paths(data, "/srv/app")
    assert result == {
        "outer_log": "/srv/app/logs/out.log",
        "nested": {
            "inner_cfg": "/srv/app/cfg/inner.toml",
            "label": "MYLABEL",
        },
    }
