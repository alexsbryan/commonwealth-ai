"""Config applier — four independent utilities for working with
configuration dictionaries.

Each function below has a subtle bug. The integration test suite
in tests/test_integration.py asserts the spec from prompt.md;
failing tests will point at the buggy function.
"""

from __future__ import annotations

import re
from typing import Any


# ── deep_merge ────────────────────────────────────────────────────


def deep_merge(base: dict, overlay: dict) -> dict:
    """Recursively merge `overlay` into `base`. Returns a new dict."""
    result = {}
    for key in base:
        result[key] = base[key]
    for key, overlay_val in overlay.items():
        if key in result:
            base_val = result[key]
            if isinstance(base_val, dict) and isinstance(overlay_val, dict):
                result[key] = deep_merge(base_val, overlay_val)
            elif isinstance(base_val, list) and isinstance(overlay_val, list):
                # BUG: spec says lists are replaced; this concatenates.
                result[key] = base_val + overlay_val
            else:
                result[key] = overlay_val
        else:
            result[key] = overlay_val
    return result


# ── expand_env ────────────────────────────────────────────────────


_ENV_PATTERN = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}")


def expand_env(value: str, env: dict) -> str:
    """Replace ${KEY} occurrences in `value` with env[KEY]."""

    def _sub(match: re.Match) -> str:
        key = match.group(1)
        if key in env:
            return str(env[key])
        return match.group(0)

    # BUG: spec says substitution recurses until a fixpoint; this
    # performs one pass only.
    return _ENV_PATTERN.sub(_sub, value)


# ── validate_schema ───────────────────────────────────────────────


def validate_schema(data: dict, schema: dict) -> None:
    """Validate `data` against `schema` ({key: type}).

    Raises ValueError on type mismatch or missing required key.
    Returns None on success.
    """
    # BUG: spec says every key in schema must be present in data,
    # but this loop iterates `data` and only checks types, never
    # noticing keys present in schema but absent from data.
    for key, value in data.items():
        if key in schema:
            expected_type = schema[key]
            if not isinstance(value, expected_type):
                raise ValueError(
                    f"type mismatch at {key}: expected "
                    f"{expected_type.__name__}, got "
                    f"{type(value).__name__}"
                )


# ── normalize_paths ───────────────────────────────────────────────


_PATH_SUFFIXES = (".txt", ".json", ".yaml", ".toml", ".md", ".cfg")


def _looks_like_relative_path(value: str) -> bool:
    if value.startswith("/"):
        return False
    if "/" in value:
        return True
    return value.endswith(_PATH_SUFFIXES)


def normalize_paths(data: dict, root: str) -> dict:
    """Walk `data` recursively; prepend `root + '/'` to relative paths."""
    result: dict = {}
    for key, value in data.items():
        if isinstance(value, dict):
            result[key] = normalize_paths(value, root)
        elif isinstance(value, str):
            # BUG: spec says only path-shaped strings get prepended,
            # but this prepends `root + "/"` to EVERY string value.
            result[key] = root + "/" + value
        elif isinstance(value, list):
            # Lists are walked but their string contents are not
            # path-normalized. Nested dicts inside lists are still
            # normalized.
            new_list = []
            for item in value:
                if isinstance(item, dict):
                    new_list.append(normalize_paths(item, root))
                else:
                    new_list.append(item)
            result[key] = new_list
        else:
            result[key] = value
    return result
