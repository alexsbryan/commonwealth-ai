"""Equal, bounded source access for every arm; every request stays at BASE."""
import subprocess
from pathlib import PurePosixPath

import common as C

MAX_TOOL_REQUESTS = 6
MAX_GENERATIONS = 8


def schema():
    def arm(action, props):
        fields = {"action": {"const": action}, **props}
        return {"type": "object", "properties": fields,
                "required": list(fields), "additionalProperties": False}
    return {"oneOf": [
        arm("read", {"path": {"type": "string", "minLength": 1},
                     "start": {"type": "integer", "minimum": 1},
                     "end": {"type": "integer", "minimum": 1}}),
        arm("search", {"query": {"type": "string", "minLength": 2, "maxLength": 100}}),
        arm("list", {"prefix": {"type": "string"}}),
    ]}


def permitted(path):
    p = PurePosixPath(path)
    return (not p.is_absolute() and ".." not in p.parts and not any(c in path for c in "\n\0:")
            and p.suffix in (".rs", ".py", ".toml", ".md", ".json")
            and not any(part in (".canon", "research", "gym", "report-audit") for part in p.parts)
            and (".claude" not in p.parts or path == ".claude/settings.json"))


def lookup(case, action):
    base = case["provenance"]["base_sha"]
    if set(action) - {"action", "path", "start", "end", "query", "prefix"}:
        raise ValueError("lookup cannot supply a revision or alternate root")
    if not C.resolves(base):
        raise ValueError("BASE is unavailable")
    if action["action"] == "read":
        path, start, end = action["path"], action["start"], action["end"]
        if not permitted(path) or not 1 <= start <= end or end - start >= 120:
            raise ValueError("read must select at most 120 source lines inside BASE")
        observed = C.P.read_window(base, path, start, end)
        return {"base": base, "path": path, "lines": [start, end], "blob": observed["git_blob"],
                "sha256": observed["window_sha256"], "text": observed["code"], "receipt": observed["receipt_id"]}
    if action["action"] == "list":
        prefix = action["prefix"]
        if prefix.startswith("/") or ".." in PurePosixPath(prefix).parts:
            raise ValueError("list prefix must stay in BASE")
        rc, files, error = C.git("ls-tree", "-r", "--name-only", base)
        if rc:
            raise ValueError(error)
        paths = [p for p in files.splitlines() if p.startswith(prefix) and permitted(p)]
        return {"base": base, "paths": paths[:60], "total": len(paths), "truncated": len(paths) > 60}
    if action["action"] == "search":
        query = action["query"]
        if not 2 <= len(query) <= 100 or "\n" in query or "\0" in query:
            raise ValueError("search needs a bounded literal query")
        proc = subprocess.run(["git", "grep", "-n", "-i", "-F", "-e", query, base,
                               "--", "*.rs", "*.py", "*.toml", "*.md", "*.json"], cwd=C.REPO,
                              capture_output=True, text=True, timeout=20)
        if proc.returncode not in (0, 1):
            raise ValueError(proc.stderr)
        hits = []
        for line in proc.stdout.splitlines():
            parts = line.split(":", 3)
            if len(parts) == 4 and permitted(parts[1]):
                hits.append({"path": parts[1], "line": int(parts[2]), "text": parts[3][:500]})
        return {"base": base, "hits": hits[:20], "total": len(hits), "truncated": len(hits) > 20}
    raise ValueError("unknown historical lookup action")
