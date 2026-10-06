#!/usr/bin/env python3
"""Bounded repository reads pinned to an immutable Git BASE revision."""
from __future__ import annotations

import hashlib
import json
import re
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
MAX_LOOKUP_RESULTS = 20
MAX_SEARCH_RESULTS = 20
MAX_SEARCH_QUERY = 200
MAX_WINDOW_LINES = 120
MAX_WINDOW_BYTES = 24_000
_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def _git(args: list[str], timeout: float = 20.0) -> bytes:
    try:
        proc = subprocess.run(["git", *args], cwd=REPO, capture_output=True,
                              timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ValueError(f"BASE repository read failed: {exc}") from exc
    if proc.returncode:
        detail = proc.stderr.decode("utf-8", "replace").strip()
        raise ValueError(f"BASE repository read failed: {detail or proc.returncode}")
    return proc.stdout


def canonical_base(base: str) -> str:
    if not isinstance(base, str) or not _SHA_RE.fullmatch(base):
        raise ValueError("BASE must be a full 40-character commit sha")
    resolved = _git(["rev-parse", "--verify", f"{base}^{{commit}}"])
    commit = resolved.decode("ascii", "strict").strip()
    if commit != base:
        raise ValueError("BASE must name the resolved commit exactly")
    return commit


def _repo_path(path: str) -> str:
    if (not isinstance(path, str) or not path or path.startswith("/")
            or "\\" in path or "\x00" in path):
        raise ValueError("path must be a nonempty repo-relative path")
    parts = path.split("/")
    if any(part in ("", ".", "..") for part in parts):
        raise ValueError("path must not contain empty, '.' or '..' components")
    return path


def _entry(base: str, path: str) -> tuple[str, str]:
    raw = _git(["ls-tree", "-r", "-z", "--full-tree", base, "--",
                f":(literal){path}"])
    for item in raw.split(b"\0"):
        if not item:
            continue
        metadata, separator, encoded_path = item.partition(b"\t")
        if not separator:
            continue
        entry_path = encoded_path.decode("utf-8", "strict")
        if entry_path != path:
            continue
        mode, kind, object_id = metadata.decode("ascii").split(" ", 2)
        if kind != "blob" or mode == "120000":
            raise ValueError("READ supports tracked regular-file blobs only")
        return object_id, mode
    raise ValueError(f"{path!r} is not a file in BASE {base[:12]}")


def _blob(base: str, path: str) -> tuple[bytes, str, str]:
    object_id, _mode = _entry(base, path)
    data = _git(["show", "--no-ext-diff", "--no-textconv", f"{base}:{path}"])
    return data, object_id, hashlib.sha256(data).hexdigest()


def _receipt_id(payload: dict) -> str:
    encoded = json.dumps(payload, ensure_ascii=False, sort_keys=True,
                         separators=(",", ":")).encode("utf-8")
    return "read-" + hashlib.sha256(encoded).hexdigest()[:16]


def read_window(base: str, path: str, line_start: int,
                line_end: int) -> dict:
    """Return the complete, untrimmed source window or reject it."""
    base = canonical_base(base)
    path = _repo_path(path)
    if (type(line_start) is not int or type(line_end) is not int
            or line_start < 1 or line_end < line_start):
        raise ValueError("READ line range must be inclusive, 1-based and ordered")
    if line_end - line_start + 1 > MAX_WINDOW_LINES:
        raise ValueError(f"READ window exceeds {MAX_WINDOW_LINES} lines")

    source, object_id, source_sha = _blob(base, path)
    lines = source.splitlines(keepends=True)
    if not lines:
        raise ValueError("READ cannot return an empty source file")
    if line_end > len(lines):
        raise ValueError(f"READ line_end {line_end} exceeds BASE file length {len(lines)}")
    window = b"".join(lines[line_start - 1:line_end])
    if not any(line.strip() for line in lines[line_start - 1:line_end]):
        raise ValueError("READ window contains no nonempty source line")
    if len(window) > MAX_WINDOW_BYTES:
        raise ValueError(f"READ window exceeds {MAX_WINDOW_BYTES} source bytes")
    try:
        code = window.decode("utf-8", "strict")
    except UnicodeDecodeError as exc:
        raise ValueError("READ window is not UTF-8 source text") from exc
    payload = {
        "action": "READ", "base_sha": base, "path": path,
        "line_start": line_start, "line_end": line_end,
        "git_blob": object_id, "source_sha256": source_sha,
        "window_sha256": hashlib.sha256(window).hexdigest(), "code": code,
    }
    return {**payload, "receipt_id": _receipt_id(payload)}


def lookup(base: str, query: str) -> dict:
    base = canonical_base(base)
    if not isinstance(query, str) or len(query) > MAX_SEARCH_QUERY:
        raise ValueError(f"LOOKUP query must be text of at most {MAX_SEARCH_QUERY} characters")
    paths = _git(["ls-tree", "-r", "-z", "--name-only", "--full-tree", base])
    matches = [p.decode("utf-8", "strict") for p in paths.split(b"\0") if p
               and query.casefold() in p.decode("utf-8", "strict").casefold()]
    limited = matches[:MAX_LOOKUP_RESULTS]
    payload = {"action": "LOOKUP", "base_sha": base, "query": query,
               "paths": limited, "truncated": len(matches) > len(limited)}
    return {**payload, "receipt_id": _receipt_id(payload)}


def search(base: str, query: str) -> dict:
    base = canonical_base(base)
    if (not isinstance(query, str) or not query.strip()
            or len(query) > MAX_SEARCH_QUERY or "\n" in query or "\r" in query):
        raise ValueError(f"SEARCH needs a literal query of 1-{MAX_SEARCH_QUERY} characters")
    try:
        proc = subprocess.run(
            ["git", "grep", "-n", "-I", "-F", "--no-color", "-e", query,
             base, "--"], cwd=REPO, capture_output=True, timeout=30, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ValueError(f"BASE SEARCH failed: {exc}") from exc
    if proc.returncode not in (0, 1):
        detail = proc.stderr.decode("utf-8", "replace").strip()
        raise ValueError(f"BASE SEARCH failed: {detail or proc.returncode}")

    matches = []
    blobs: dict[str, tuple[bytes, str, str]] = {}
    prefix = (base + ":").encode("ascii")
    for raw_line in proc.stdout.splitlines():
        if not raw_line.startswith(prefix):
            continue
        location = raw_line[len(prefix):].decode("utf-8", "strict")
        try:
            path, number, _matched_text = location.rsplit(":", 2)
            line_number = int(number)
        except (ValueError, TypeError):
            continue
        if path not in blobs:
            blobs[path] = _blob(base, path)
        source, object_id, source_sha = blobs[path]
        source_lines = source.splitlines(keepends=True)
        if not 1 <= line_number <= len(source_lines):
            continue
        text = source_lines[line_number - 1].decode("utf-8", "strict")
        matches.append({"path": path, "line": line_number,
                        "text": text, "git_blob": object_id,
                        "source_sha256": source_sha})
        if len(matches) > MAX_SEARCH_RESULTS:
            break
    limited = matches[:MAX_SEARCH_RESULTS]
    payload = {"action": "SEARCH", "base_sha": base, "query": query,
               "matches": limited, "truncated": len(matches) > len(limited)}
    return {**payload, "receipt_id": _receipt_id(payload)}


def execute(base: str, action: dict) -> dict:
    if not isinstance(action, dict):
        raise ValueError("tool action must be an object")
    kind = action.get("action")
    if kind == "LOOKUP":
        return lookup(base, action.get("query"))
    if kind == "SEARCH":
        return search(base, action.get("query"))
    if kind == "READ":
        return read_window(base, action.get("path"), action.get("line_start"),
                           action.get("line_end"))
    raise ValueError(f"unsupported repository action {kind!r}")


def verify_read_receipt(receipt: dict, expected_base: str) -> bool:
    if not isinstance(receipt, dict) or receipt.get("action") != "READ":
        return False
    if receipt.get("base_sha") != expected_base:
        return False
    try:
        rebuilt = read_window(expected_base, receipt.get("path"),
                              receipt.get("line_start"), receipt.get("line_end"))
    except (ValueError, TypeError):
        return False
    return rebuilt == receipt
