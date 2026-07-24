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
import subprocess
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


def check_harness_config(rep):
    """0. Harness-side toolbox config — does the AGENT even have the tools?

    Root-caused 2026-07-23: the sovereign server was declared under
    `mcpServers` in `.claude/settings.json` — a key Claude Code does not read —
    so NO session ever surfaced the MCP tools, while every daemon-side check
    here passed. The daemon being healthy is worthless if the harness never
    mounts the toolbox. Canonical config: `.mcp.json` at repo root, approved
    via `enabledMcpjsonServers` / `enableAllProjectMcpServers` in settings.
    This check is filesystem-only and runs even when the daemon is down.
    """
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.abspath(os.path.join(here, ".."))
    mcp_json = os.path.join(root, ".mcp.json")
    settings_path = os.path.join(root, ".claude", "settings.json")

    server_names = []
    try:
        with open(mcp_json) as f:
            servers = json.load(f).get("mcpServers", {})
        server_names = list(servers.keys())
        if server_names:
            rep.add("PASS", "harness .mcp.json", f"declares: {', '.join(server_names)}")
        else:
            rep.add("FAIL", "harness .mcp.json", "exists but declares no servers",
                    "add the sovereign HTTP server to .mcp.json at the repo root")
    except FileNotFoundError:
        rep.add("FAIL", "harness .mcp.json", "MISSING — agents have NO MCP tools in-session",
                'create .mcp.json at repo root: {"mcpServers": {"sovereign": '
                '{"type": "http", "url": "http://localhost:9741/mcp"}}}')
    except Exception as e:
        rep.add("FAIL", "harness .mcp.json", f"unparseable: {e}", "fix the JSON")

    try:
        with open(settings_path) as f:
            settings = json.load(f)
        if "mcpServers" in settings:
            rep.add("WARN", "harness settings.json", "dead `mcpServers` key present (ignored by "
                    "Claude Code — servers belong in .mcp.json)",
                    "remove it so the config can't masquerade as working")
        enabled = settings.get("enabledMcpjsonServers", [])
        enable_all = settings.get("enableAllProjectMcpServers", False)
        unapproved = [s for s in server_names if s not in enabled and not enable_all]
        if server_names and unapproved:
            rep.add("WARN", "harness mcp approval",
                    f"not durably enabled in settings: {', '.join(unapproved)}",
                    'add "enabledMcpjsonServers": [...] to .claude/settings.json '
                    "(each user must also trust the workspace once)")
        elif server_names:
            rep.add("PASS", "harness mcp approval", "enabled in project settings")
    except FileNotFoundError:
        pass  # no project settings — approval is per-user; nothing to verify
    except Exception as e:
        rep.add("WARN", "harness settings.json", f"unparseable: {e}", "fix the JSON")


def check_continuity_local(rep, cont):
    """4. Session-continuity stack, local half — filesystem + CLI, daemon-independent.

    The continuity protocol (frames, split-enforce, distill) is repo-committed
    config calling machine-local binaries. A teammate who pulls the hooks but
    runs stale binaries degrades SILENTLY: session-frame.sh logs-and-exits-0,
    frames never bank, and an old distill clobbers self-reported frames. These
    checks catch the skew loudly, per machine.
    """
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.abspath(os.path.join(here, ".."))

    # a. Binary new enough — the distill provenance guard ships --force.
    try:
        out = subprocess.run(["sovereign", "session", "distill", "--help"],
                             capture_output=True, text=True, timeout=10)
        if "--force" in (out.stdout + out.stderr):
            rep.add("PASS", "distill guard", "self-reported frames are protected (--force present)")
        else:
            rep.add("FAIL", "distill guard", "sovereign binary predates the provenance guard — "
                    "SessionEnd distill will CLOBBER banked frames",
                    "(cd sovereign && cargo build --features sovereign-cli/dev-tools -p sovereign-cli) "
                    "and re-point the ~/.local/bin/sovereign symlink")
    except FileNotFoundError:
        rep.add("FAIL", "sovereign on PATH", "`sovereign` not found — every hook is a silent no-op",
                "ln -sf $(realpath sovereign/target/*/sovereign-cli) ~/.local/bin/sovereign")
    except Exception as e:
        rep.add("WARN", "distill guard", f"could not probe the CLI: {e}")

    # b. Hook files present (repo-committed; missing means partial checkout).
    hooks_dir = os.path.join(root, ".claude", "hooks")
    missing = [h for h in cont.get("hook_files", [])
               if not os.path.isfile(os.path.join(hooks_dir, h))]
    if missing:
        rep.add("FAIL", "continuity hooks", f"missing from .claude/hooks: {', '.join(missing)}",
                "git pull / restore — the split+frame protocol is off without them")
    elif cont.get("hook_files"):
        rep.add("PASS", "continuity hooks", f"{len(cont['hook_files'])} hook files present")

    # c. Hook + statusline commands must be cwd-independent. A cwd-relative
    # path wedges every Bash call the moment the shell cwd drifts off the
    # repo root (observed live 2026-07-24).
    settings_path = os.path.join(root, ".claude", "settings.json")
    try:
        with open(settings_path) as f:
            settings = json.load(f)
        cmds = []
        for entries in (settings.get("hooks") or {}).values():
            for entry in entries if isinstance(entries, list) else []:
                for h in entry.get("hooks", []):
                    cmds.append(h.get("command", ""))
        sl = (settings.get("statusLine") or {}).get("command")
        if sl:
            cmds.append(sl)
        rel = [c for c in cmds
               if ".claude/" in c and "$CLAUDE_PROJECT_DIR" not in c and not c.strip().startswith("/")]
        if rel:
            rep.add("WARN", "cwd-relative commands", f"{len(rel)} hook/statusline command(s) not "
                    "anchored — wedge Bash when shell cwd leaves the repo root",
                    'prefix with "$CLAUDE_PROJECT_DIR"/ in .claude/settings.json')
        elif cmds:
            rep.add("PASS", "hook path anchoring", f"{len(cmds)} command(s) cwd-independent")
    except FileNotFoundError:
        pass  # already reported by check_harness_config
    except Exception:
        pass

    # d. Frame store writable — where session_state banks and boot injection reads.
    sessions_dir = os.path.expanduser(cont.get("sessions_dir", "~/.sovereign/sessions"))
    probe = os.path.join(sessions_dir, ".preflight-probe")
    try:
        os.makedirs(sessions_dir, exist_ok=True)
        with open(probe, "w") as f:
            f.write("ok")
        os.remove(probe)
        rep.add("PASS", "frame store", f"{sessions_dir} writable")
    except OSError as e:
        rep.add("FAIL", "frame store", f"{sessions_dir} not writable: {e}",
                "frames cannot bank; fix permissions")


def check_peer(rep, host, name, expected_tools):
    """5. Peer-node verification over the mesh — read-only HTTP, no SSH.

    Same-mesh machines are directly probe-able (:9741 over the tailnet), so
    the coordinator can VERIFY a peer's daemon actually serves the expected
    tool roster instead of hoping its operator rebuilt after pulling.
    Unreachable is WARN (laptops sleep); reachable-but-stale is FAIL — that
    peer's agents are running a different protocol than ours.
    """
    base = host if ":" in host else f"{host}:9741"
    label = f"peer:{name}"
    status = http_get_json(f"http://{base}/status", timeout=6)
    if status is None:
        rep.add("WARN", label, f"{base} unreachable — cannot verify (offline/asleep?)",
                f"re-check when it's up: agent-preflight --peer {host}")
        return
    node = str(status.get("node_id", "?"))[:12]
    try:
        result, error = mcp_call(f"http://{base}/mcp", "tools/list", {}, timeout=10)
    except Exception as e:
        rep.add("FAIL", label, f"node {node}: /status OK but MCP dead: {e}",
                "daemon half-up on the peer; its operator should `sovereign daemon restart`")
        return
    if error:
        rep.add("FAIL", label, f"node {node}: tools/list error: {error}",
                "peer daemon unhealthy; its operator should check `sovereign doctor`")
        return
    present = {t.get("name") for t in (result or {}).get("tools", [])}
    missing = [t for t in expected_tools if t not in present]
    if missing:
        rep.add("FAIL", label, f"node {node}: missing {', '.join(missing)} — daemon is STALE",
                "peer must rebuild + `sovereign daemon restart` (no SSH from here; "
                "ask that machine's operator)")
    else:
        rep.add("PASS", label, f"node {node}: {len(expected_tools)} expected tools live")


def run(golden, peers=None, skip_peers=False):
    rep = Report()
    status_url = golden["status_url"]
    mcp_url = golden["mcp_url"]
    cont = golden.get("continuity", {})

    # 0. Agent-side toolbox config — independent of daemon liveness.
    check_harness_config(rep)

    # 4. Continuity stack, local half — also daemon-independent; run it before
    # any early return below so a down daemon doesn't hide binary/hook skew.
    check_continuity_local(rep, cont)

    # 5. Peer verification — network-only, independent of the local daemon.
    if not skip_peers:
        peer_expected = golden["expected_mcp_tools"] + cont.get("expected_mcp_tools", [])
        for p in (peers if peers is not None else cont.get("peers", [])):
            check_peer(rep, p["host"], p.get("name", p["host"]), peer_expected)

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

    cont_missing = [t for t in cont.get("expected_mcp_tools", []) if t not in present]
    if cont_missing:
        rep.add("FAIL", "continuity MCP tools",
                f"missing: {', '.join(cont_missing)} — encode-time frames cannot bank",
                "local daemon predates the tool; rebuild + `sovereign daemon restart`")
    elif cont.get("expected_mcp_tools"):
        rep.add("PASS", "continuity MCP tools", f"{', '.join(cont['expected_mcp_tools'])} live")

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
    skip_peers = "--no-peers" in argv
    golden_path = None
    extra_peers = []
    for i, a in enumerate(argv):
        if a == "--golden" and i + 1 < len(argv):
            golden_path = argv[i + 1]
        if a == "--peer" and i + 1 < len(argv):
            extra_peers.append({"host": argv[i + 1], "name": argv[i + 1]})
    if golden_path is None:
        here = os.path.dirname(os.path.abspath(__file__))
        golden_path = os.path.join(here, "..", "quality", "agent-preflight.golden.json")

    try:
        golden = load_golden(golden_path)
    except Exception as e:
        print(f"agent-preflight: cannot load golden set {golden_path}: {e}", file=sys.stderr)
        return 2

    peers = None
    if extra_peers:
        peers = golden.get("continuity", {}).get("peers", []) + extra_peers
    rep = run(golden, peers=peers, skip_peers=skip_peers)
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
