#!/usr/bin/env python3
"""agent-preflight — is the code-intelligence / RAG surface actually ready?

WHY. An agent that hits a broken or empty code-intelligence tool does not slow
down — it silently abandons the distilled path and reverts to `grep`/`cat` for
the rest of the session (or gives up). So "the server is up" is worthless as a
readiness signal; what matters is whether a KNOWN-GOOD query returns a real
answer. This script exercises the SAME path an agent uses (the MCP server at
:9741) with a versioned golden set and fails loudly, per-tool, when the surface
has regressed — the stale-index / missing-tool / empty-store failures that make
agents bail.

Runnable three ways: by a harness at session start, by CI to catch a regression
before it reaches the fleet, or by an agent the moment a tool "feels" wrong.

Exit codes: 0 = all green (WARN/DEGRADED allowed), 1 = at least one FAIL,
2 = usage/config error. `--strict` also fails on DEGRADED.
"""

import json
import os
import sys
import urllib.request
import urllib.error

RESET = "\033[0m"
COLOR = {"PASS": "\033[32m", "WARN": "\033[33m", "DEGRADED": "\033[33m", "FAIL": "\033[31m"}


def load_golden(path):
    with open(path) as f:
        return json.load(f)


def http_get_status(url, timeout=5):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except Exception:
        return None


def http_get_json(url, timeout=5):
    """GET a JSON body, or None on any error."""
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return json.loads(r.read().decode())
    except Exception:
        return None


def mcp_call(mcp_url, method, params, timeout=15):
    """Return (result_obj, error_obj) — one is None. Raises on transport error."""
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(mcp_url, data=body, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        payload = json.loads(r.read().decode())
    return payload.get("result"), payload.get("error")


def result_text(result):
    """Flatten an MCP tools/call result's content blocks to text."""
    if not result:
        return ""
    blocks = result.get("content", [])
    return "\n".join(b.get("text", "") for b in blocks if isinstance(b, dict))


class Report:
    def __init__(self):
        self.rows = []  # (status, check, detail, remedy)

    def add(self, status, check, detail, remedy=""):
        self.rows.append((status, check, detail, remedy))

    def any_fail(self):
        return any(s == "FAIL" for s, *_ in self.rows)

    def any_degraded(self):
        return any(s == "DEGRADED" for s, *_ in self.rows)

    def print_table(self, color):
        print(f"{'':<10}{'check':<26}detail")
        print("-" * 78)
        for status, check, detail, remedy in self.rows:
            tag = f"{COLOR[status]}{status}{RESET}" if color else status
            print(f"{tag:<{19 if color else 10}}{check:<26}{detail}")
            if remedy and status in ("FAIL", "DEGRADED", "WARN"):
                print(f"{'':<10}{'':<26}→ {remedy}")
        print("-" * 78)
        n_fail = sum(1 for s, *_ in self.rows if s == "FAIL")
        n_deg = sum(1 for s, *_ in self.rows if s in ("DEGRADED", "WARN"))
        n_pass = sum(1 for s, *_ in self.rows if s == "PASS")
        verdict = "READY" if n_fail == 0 else "NOT READY"
        print(f"{verdict}: {n_pass} pass, {n_deg} warn/degraded, {n_fail} fail")

    def to_json(self):
        return json.dumps([
            {"status": s, "check": c, "detail": d, "remedy": r} for s, c, d, r in self.rows
        ])


def run(golden):
    rep = Report()
    status_url = golden["status_url"]
    mcp_url = golden["mcp_url"]

    # 1. Daemon reachable — the whole surface lives behind :9741.
    status = http_get_json(status_url)
    if status is not None:
        rep.add("PASS", "daemon reachable", f"{status_url} -> 200")
    else:
        rep.add("FAIL", "daemon reachable", f"{status_url} -> no JSON response",
                "start it: `sovereign daemon start` (inside the dev-toolbox)")
        # Nothing else can work; short-circuit the MCP checks.
        return rep

    # 1b. Searchable index populated — a portable freshness/population signal.
    # `sovereign project refresh` has been observed to WIPE the index to 0 when
    # its SCIP exporters are missing/broken while still reporting success, so an
    # emptied index must fail loudly here, not read as "no matches".
    knowledge = status.get("knowledge") or {}
    chunks = knowledge.get("total_chunks_searchable")
    corpora = knowledge.get("hosted_corpora")
    if isinstance(chunks, int):
        if chunks <= 0:
            rep.add("FAIL", "searchable index", "0 chunks searchable — index is EMPTY",
                    "index wiped or never built; `sovereign project refresh` (ensure "
                    "rust-analyzer/scip exporters are installed first — a failed export "
                    "can clear the index)")
        else:
            n_corp = len(corpora) if isinstance(corpora, list) else corpora
            rep.add("PASS", "searchable index", f"{chunks:,} chunks across {n_corp} corpora")

    # 2. MCP tools/list — the tools an agent can actually see.
    try:
        result, error = mcp_call(mcp_url, "tools/list", {})
    except Exception as e:
        rep.add("FAIL", "mcp tools/list", f"transport error: {e}",
                "MCP endpoint not serving; check `sovereign doctor`")
        return rep
    if error:
        rep.add("FAIL", "mcp tools/list", f"{error}", "check `sovereign doctor`")
        return rep
    present = {t.get("name") for t in (result or {}).get("tools", [])}
    missing = [t for t in golden["expected_mcp_tools"] if t not in present]
    if missing:
        rep.add("FAIL", "expected MCP tools", f"missing: {', '.join(missing)}",
                "in mcp_surface.rs::MCP_TOOLS_ALWAYS but not on the live surface — "
                "deployed daemon is stale (rebuild + restart), or it was removed from MCP")
    else:
        rep.add("PASS", "expected MCP tools", f"{len(golden['expected_mcp_tools'])} present")

    cli_only = [t for t in golden.get("known_cli_only", {}).get("tools", []) if t not in present]
    if cli_only:
        rep.add("WARN", "not on MCP surface", f"{', '.join(cli_only)} are CLI-only",
                "agents on MCP can't reach these; expose a distilled variant in MCP_TOOLS_ALWAYS")

    # 3. Golden round-trips — a known-good query MUST return a real answer.
    for spec in golden["golden"].get("symbols", []):
        name = spec["name"]
        txt = _safe_call(rep, mcp_url, "symbols", spec, f"symbols({name})")
        if txt is None:
            continue
        low = txt.lower()
        if "no symbol" in low or "not found" in low or not txt.strip():
            rep.add("FAIL", f"symbols({name})", "returned no definition",
                    "index regressed; `sovereign project refresh` then re-index if needed")
        elif "couldn't read source" in low or "could not read source" in low:
            rep.add("DEGRADED", f"symbols({name})", "resolved but source unreadable (STALE index)",
                    "SCIP index points at a moved/deleted path; `sovereign project refresh`")
        else:
            rep.add("PASS", f"symbols({name})", "resolved with source")

    for spec in golden["golden"].get("callers", []):
        sym = spec["symbol"]
        txt = _safe_call(rep, mcp_url, "callers", spec, f"callers({sym})")
        if txt is None:
            continue
        low = txt.lower()
        if "invalid input" in low or "requires" in low:
            rep.add("FAIL", f"callers({sym})", f"param contract mismatch: {txt.strip()[:80]}",
                    "the golden's arg keys don't match the tool schema; fix golden or tool")
        elif "no callers" in low or not txt.strip():
            rep.add("DEGRADED", f"callers({sym})", "no callers found for a known-called symbol",
                    "call graph may be stale; `sovereign project refresh`")
        else:
            rep.add("PASS", f"callers({sym})", "returned call sites")

    nq = golden["golden"].get("notes_query")
    if nq:
        txt = _safe_call(rep, mcp_url, "notes", {"query": nq}, f"notes({nq})")
        if txt is not None:
            got = _notes_count(txt)
            if got > 0:
                rep.add("PASS", f"notes({nq})", f"{got} note(s) — external brain populated")
            else:
                rep.add("DEGRADED", f"notes({nq})", "empty result",
                        "notes.db unreachable or unpopulated; the external brain is dark")

    return rep


def _safe_call(rep, mcp_url, tool, args, label):
    try:
        result, error = mcp_call(mcp_url, "tools/call", {"name": tool, "arguments": args})
    except Exception as e:
        rep.add("FAIL", label, f"transport error: {e}", "check daemon / `sovereign doctor`")
        return None
    if error:
        rep.add("FAIL", label, f"{error.get('message', error)}",
                "tool not found or errored on the MCP surface")
        return None
    if result and result.get("isError"):
        return result_text(result)  # tool-level error text; classified by caller
    return result_text(result)


def _notes_count(txt):
    try:
        obj = json.loads(txt)
        return len(obj.get("notes", []))
    except Exception:
        # Non-JSON note render — count non-empty lines as a rough floor.
        return 1 if txt.strip() else 0


def main(argv):
    strict = "--strict" in argv
    as_json = "--json" in argv
    color = sys.stdout.isatty() and "--no-color" not in argv
    golden_path = None
    for i, a in enumerate(argv):
        if a == "--golden" and i + 1 < len(argv):
            golden_path = argv[i + 1]
    if golden_path is None:
        here = os.path.dirname(os.path.abspath(__file__))
        golden_path = os.path.join(here, "..", "quality", "agent-preflight.golden.json")

    try:
        golden = load_golden(golden_path)
    except Exception as e:
        print(f"agent-preflight: cannot load golden set {golden_path}: {e}", file=sys.stderr)
        return 2

    rep = run(golden)
    if as_json:
        print(rep.to_json())
    else:
        rep.print_table(color)

    if rep.any_fail():
        return 1
    if strict and rep.any_degraded():
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
