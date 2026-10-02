#!/bin/bash
# The boot hook's brain line tells three answers apart: tools live, slow, and
# absent. Until 2026-09-26 it gated on a 2 s GET /status, whose handler makes
# cw-rails round trips, and it called a slow-but-up daemon "not reachable".
# Each case runs the REAL hook against a stand-in on a free port.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
ROOT="$(mktemp -d)"
export SOVEREIGN_SESSIONS_DIR="$ROOT/sessions" SOVEREIGN_LINEAGE_DIR="$ROOT/lineage"
mkdir -p "$SOVEREIGN_SESSIONS_DIR" "$SOVEREIGN_LINEAGE_DIR"

pass=0; fail=0
check() { if printf '%s' "$3" | grep -q -- "$2"; then echo "  ok   $1"; pass=$((pass+1));
          else echo "  FAIL $1: wanted /$2/ in [$3]"; fail=$((fail+1)); fi; }
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
brain() { # brain <port> -> the hook's _brain: line
  printf '{"session_id":"brain-probe","source":"startup","cwd":"%s"}' "$PWD" \
    | SOVEREIGN_PORT="$1" sh .claude/hooks/session-boot.sh | grep '^_brain:'
}
stand_in() { # stand_in <port> <delay_s>: answers /mcp tools/list with two tools after a delay
  # stdout goes to /dev/null: `pid=$(stand_in …)` waits on every writer of its
  # pipe, and a server that inherited it would hold the substitution open forever.
  python3 - "$1" "$2" >/dev/null 2>&1 <<'PY' &
import http.server, json, sys, time
port, delay = int(sys.argv[1]), float(sys.argv[2])
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        time.sleep(delay)
        body = json.dumps({"jsonrpc": "2.0", "id": 1,
                           "result": {"tools": [{"name": "symbols"}, {"name": "callers"}]}}).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", port), H).serve_forever()
PY
  echo $!
}

echo "brain-probe: the boot hook's code-intel line"
p=$(free_port)
check "nothing listening reads as absent" "not reachable on :$p" "$(brain "$p")"

p=$(free_port); pid=$(stand_in "$p" 0); sleep 0.5
check "a live /mcp reads as tools live" "daemon up · 2 MCP tools live" "$(brain "$p")"
kill "$pid" 2>/dev/null

p=$(free_port); pid=$(stand_in "$p" 6); sleep 0.5
out="$(brain "$p")"
check "a slow /mcp reads as slow, never absent" "did not answer /mcp within 4s" "$out"
if printf '%s' "$out" | grep -q 'code intel is dark'; then echo "  FAIL a slow /mcp is called dark: [$out]"; fail=$((fail+1));
else echo "  ok   a slow /mcp is not called dark"; pass=$((pass+1)); fi
kill "$pid" 2>/dev/null

rm -rf "$ROOT"
echo "brain-probe: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
