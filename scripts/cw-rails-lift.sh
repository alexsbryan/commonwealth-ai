#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-rails-lift.sh — the instrument for the rails lift (cw-lift D1 follow-on).
# Twin of `scripts/cw-work-lift.sh`: the work plane's lift proved a package-only
# peer holding a roster key and serving no HTTP; this one proves a package-only
# DAEMON — one that joins a mesh by invite, gossips, answers a peer's dial, and
# hands a player a loopback URL — built and RUN outside this monorepo.
#
#   scripts/cw-rails-lift.sh --sandbox [--invite <link>] [--dir <path>] [--keep]
#
# THE BAR IS A PHYSICAL LIFT, NOT A CRATE-NAME COUNT. `cargo tree` cannot see a
# `build.rs`, an `include_str!` that escapes the crate root, a hand-spelled
# relative path dependency, or a test that reads the repo root through
# CARGO_MANIFEST_DIR.parent() — and each of those is a lift that fails on a
# closure the gate scores green. So this copies the closure to a scratch
# directory OUTSIDE the repository, synthesises a root workspace there, builds
# and tests it with nothing of this monorepo on the path, and then RUNS the
# binary against a real mesh.
#
# ── The four verdicts, and how the runner reads them (ARCH §18.2, §18.3) ────
#
# `scripts/co-lineage.py::measure_bar` maps an instrument's exit code and last
# stdout line onto a measurement row, so the mapping is a contract:
#
#   rc 0, stdout {"value": 1}   the lift worked — passed
#   rc 0, stdout {"value": 0}   MEASURED FAILURE — the closure did not lift, or
#                               it lifted and the daemon did not do its job
#   rc 3                        could-not-judge: a precondition of the RUN is
#                               absent. Nothing was measured.
#                               THE EXPECTED CAUSE here is NO INVITE. A rails
#                               daemon cannot found a mesh — it has no
#                               `/internal/join` and mints no invite, which is
#                               the deliberate scope that keeps it small — so
#                               step 5 needs a mesh that already exists and
#                               someone's word that this host may on it. Steps
#                               1-4 still MEASURE (the closure resolves, builds
#                               and passes its own tests outside the monorepo);
#                               only the run abstains. Reporting that as
#                               `{"value": 0}` would say the lift failed when
#                               what happened is that nobody was asked.
#                               Pass `--invite`, or set `CW_RAILS_INVITE`, or
#                               run it on a host whose `svrn mesh invite` works.
#   rc 2                        usage
#   rc 127                      instrument-missing (the runner's own reading
#                               when this file does not exist)
#
# The 0 and the 3 are the whole point of writing this carefully. A 0 written by
# a missing tool would be a substitution indistinguishable from a measured
# failure, which is exactly what the bar's own text forbids. Every abstention
# below names what was absent, on stderr, before it exits 3.
#
# NOT `set -e`: a failure here is a VERDICT to classify, never an abort.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT="$REPO/target/cw-rails-lift/last.json"
SANDBOX=""
KEEP=0
MODE=""
# The invite. NOT defaulted and NOT minted here: an invite is a member's word
# that this host may join their mesh, and an instrument that minted its own
# would be measuring a mesh of one — which is the one shape where gossip,
# offers and reach all trivially pass by having nobody to disagree with.
INVITE="${CW_RAILS_INVITE:-}"
DAEMON_PID=""
ORIGIN_PID=""

say() { printf '%s\n' "$*" >&2; }
rule() { say "── $* ─────────────────────────────────────────────" ; }

cleanup() {
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null
  [ -n "$ORIGIN_PID" ] && kill "$ORIGIN_PID" 2>/dev/null
  if [ -n "$SANDBOX" ] && [ -d "$SANDBOX" ]; then
    if [ "$KEEP" = 1 ]; then say "sandbox kept at $SANDBOX"
    else rm -rf "$SANDBOX"; fi
  fi
}

# The ONE place a verdict is emitted, so the artifact and the runner's value
# cannot disagree (ARCH §10.6). $1 value, $2 one-line reason.
verdict() {
  mkdir -p "$(dirname "$ARTIFACT")"
  # Whether an invite was present is in the row, because a green with a live
  # mesh and a green with none are different facts and a reader six weeks out
  # cannot tell them apart from a value alone.
  printf '{"value": %s, "reason": %s, "sandbox": "%s", "had_invite": %s, "at": "%s"}\n' \
    "$1" "$(printf '%s' "$2" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" \
    "$SANDBOX" "$([ -n "$INVITE" ] && echo true || echo false)" \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$ARTIFACT"
  say ""
  say "VERDICT $1 — $2"
  printf '{"value": %s, "artifact": "target/cw-rails-lift/last.json"}\n' "$1"
  cleanup
  exit 0
}

# An abstention. Distinct from `verdict 0` on purpose: it makes no claim.
abstain() {
  say ""
  say "COULD-NOT-JUDGE — $1"
  cleanup
  exit 3
}

while [ $# -gt 0 ]; do
  case "$1" in
    --sandbox) MODE=sandbox ;;
    --keep) KEEP=1 ;;
    --invite) shift; INVITE="${1:-}" ;;
    --dir) shift; SANDBOX="${1:-}" ;;
    -h|--help) sed -n '3,10p' "${BASH_SOURCE[0]}" >&2; exit 2 ;;
    *) say "unknown argument \`$1\`"; exit 2 ;;
  esac
  shift
done
[ "$MODE" = sandbox ] || { say "usage: scripts/cw-rails-lift.sh --sandbox [--invite <link>] [--dir <path>] [--keep]"; exit 2; }

# ── Preconditions of the RUN, not of the claim ─────────────────────────────
command -v cargo   >/dev/null 2>&1 || abstain "cargo is not on PATH — nothing can be built, so nothing was measured"
command -v python3 >/dev/null 2>&1 || abstain "python3 is not on PATH — the lift's own planner needs it, and so does the media origin the run stands up"
command -v curl    >/dev/null 2>&1 || abstain "curl is not on PATH — step 5 reads the daemon's own routes with it"
[ -f "$REPO/commonwealth/crates/commonwealth-rails/Cargo.toml" ] || \
  abstain "commonwealth-rails is not in this checkout at $REPO — this is not the tree the bar is about"

# The sandbox lives OUTSIDE the repository, and that is load-bearing twice
# over: cargo inherits `.cargo/config.toml` from ANY ancestor directory, and a
# sandbox under the repo would silently pick up this workspace's linker and
# rustflags — the exact monorepo dependency the lift exists to disprove.
[ -n "$SANDBOX" ] || SANDBOX="${TMPDIR:-/tmp}/cw-rails-lift.$$"
case "$SANDBOX" in "$REPO"|"$REPO"/*) say "refusing a sandbox inside the repository: $SANDBOX"; exit 2 ;; esac
rm -rf "$SANDBOX"; mkdir -p "$SANDBOX" || abstain "could not create the sandbox at $SANDBOX"

rule "1. the closure, and the hazards a crate-name count cannot see"
python3 - "$REPO" "$SANDBOX" <<'PY'
import os, re, shutil, sys, tomllib
repo, sandbox = sys.argv[1], sys.argv[2]
root = tomllib.loads(open(os.path.join(repo, "Cargo.toml")).read())
ws = root["workspace"]
wsdeps = ws.get("dependencies", {})

# `[[forbid]]` in quality/ARCH_LAYERS.toml says these may never be in the
# package's closure. Checked HERE too, because the gate reads the manifest and
# this reads what was actually copied — one of the two can be stale. The
# application crates are named explicitly: `commonwealth-api` is the one a
# well-meant commit reaches for first (it has the gossip handler and the
# fan-out route this daemon mirrors) and it carries corpus-engine.
FORBIDDEN = re.compile(r"^(sovereign-|corpus-engine|commonwealth-(knowledge|inference|api)$)")

def manifest(path): return tomllib.loads(open(path).read())
def tables(m):
    for t in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name, spec in m.get(t, {}).items():
            yield t, name, (spec if isinstance(spec, dict) else {"version": spec})

# Walk the workspace-local closure from commonwealth-rails, dev-dependencies
# included: BOUNDARY.md's rules count them, because a third party who lifts the
# package carries its tests.
seeds = {"commonwealth-rails": os.path.join(repo, "commonwealth/crates/commonwealth-rails")}
found, hand_paths, referenced, queue = {}, [], set(), list(seeds.items())
while queue:
    name, cdir = queue.pop()
    if name in found: continue
    found[name] = cdir
    for table, dep, spec in tables(manifest(os.path.join(cdir, "Cargo.toml"))):
        if FORBIDDEN.match(dep):
            print(f"FORBIDDEN {name} -> {dep} ({table})"); sys.exit(4)
        if spec.get("workspace"):
            referenced.add(dep)
            entry = wsdeps.get(dep)
            if entry is None:
                print(f"UNDECLARED {name} -> {dep}: `workspace = true` with no root entry"); sys.exit(4)
            if isinstance(entry, dict) and "path" in entry:
                queue.append((dep, os.path.join(repo, entry["path"])))
        elif "path" in spec:
            # A hand-spelled relative path hardcodes this monorepo's directory
            # depth and dies at `cargo metadata`, before a line compiles.
            # boundary-gate is blind to it — it validates that the EDGE is
            # legal, never how it is SPELLED.
            hand_paths.append(f"{name} -> {dep} = {{ path = \"{spec['path']}\" }} ({table})")

if hand_paths:
    for h in hand_paths: print("HAND-SPELLED-PATH " + h)
    sys.exit(4)

# Hazards that a green `cargo tree` and a green boundary-gate both miss.
# Reported whether or not they bite, because the build below is what decides
# and a reader wants to know what was in the sandbox either way.
hazard = re.compile(r'CARGO_MANIFEST_DIR|git\s+ls-files|include_(str|bytes)!\s*\(\s*"[^"]*\.\.')
for name, cdir in sorted(found.items()):
    if os.path.exists(os.path.join(cdir, "build.rs")):
        print(f"HAZARD {name}: build.rs (BOUNDARY.md §4 forbids it in a package crate)")
    for dirpath, dirnames, files in os.walk(cdir):
        dirnames[:] = [d for d in dirnames if d not in ("target", ".git")]
        for f in files:
            if not f.endswith(".rs"): continue
            fp = os.path.join(dirpath, f)
            for i, line in enumerate(open(fp, errors="replace"), 1):
                if hazard.search(line) and not line.lstrip().startswith(("//", "*")):
                    print(f"HAZARD {os.path.relpath(fp, repo)}:{i}: {line.strip()[:110]}")

# Copy FLAT — crates/<name> — rather than preserving the monorepo's directory
# shape. A layout-preserving sandbox proves the crates compile where they
# already are, which is not the question.
os.makedirs(os.path.join(sandbox, "crates"))
ignore = shutil.ignore_patterns("target", ".git")
for name, cdir in found.items():
    shutil.copytree(cdir, os.path.join(sandbox, "crates", name), ignore=ignore)

def render(v):
    if isinstance(v, bool): return "true" if v else "false"
    if isinstance(v, (int, float)): return str(v)
    if isinstance(v, str): return '"' + v.replace('\\', '\\\\').replace('"', '\\"') + '"'
    if isinstance(v, list): return "[" + ", ".join(render(x) for x in v) + "]"
    if isinstance(v, dict): return "{ " + ", ".join(f"{k} = {render(x)}" for k, x in v.items()) + " }"
    raise TypeError(v)

out = ['[workspace]', 'resolver = "2"', 'members = ["crates/*"]', '']
out.append("[workspace.package]")
for k, v in ws.get("package", {}).items(): out.append(f"{k} = {render(v)}")
out.append("")
for section, body in ws.get("lints", {}).items():
    out.append(f"[workspace.lints.{section}]")
    for k, v in body.items(): out.append(f"{k} = {render(v)}")
    out.append("")
out.append("[workspace.dependencies]")
for dep in sorted(referenced):
    entry = wsdeps[dep]
    if isinstance(entry, dict) and "path" in entry:
        entry = dict(entry); entry["path"] = f"crates/{dep}"
    out.append(f"{dep} = {render(entry)}")
open(os.path.join(sandbox, "Cargo.toml"), "w").write("\n".join(out) + "\n")

# The lock travels: a third party gets one from `cargo package` too, and
# without it the sandbox re-resolves from the network and the build time below
# would be measuring crates.io. Cargo prunes the entries it no longer needs.
shutil.copyfile(os.path.join(repo, "Cargo.lock"), os.path.join(sandbox, "Cargo.lock"))
print("CRATES " + " ".join(sorted(found)))
print(f"COUNT {len(found)}")
PY
rc=$?
case $rc in
  0) : ;;
  4) verdict 0 "the closure cannot leave the monorepo — see the FORBIDDEN / HAND-SPELLED-PATH / UNDECLARED line above" ;;
  *) abstain "the closure planner failed with rc $rc before anything was copied" ;;
esac

# NOT copied, deliberately, and each absence is a claim being tested:
#   .cargo/config.toml  a third party has their own linker settings
#   clippy.toml         the workspace's own lint policy is not the package's
#   rust-toolchain.toml the lift must build on a stock toolchain
say "toolchain in the sandbox: $(cd "$SANDBOX" && cargo --version 2>&1 | head -1)"
say "sandbox: $SANDBOX  (repo is $REPO — nothing under it is on this path)"

rule "2. does it resolve at all?"
# Resolution BEFORE the build, and its failure is an abstention rather than a
# verdict: a registry that cannot be reached says nothing about whether this
# closure lifts (ARCH §18.2). A build failure AFTER a clean resolve does.
if ! (cd "$SANDBOX" && RUSTC_WRAPPER= cargo metadata --format-version 1 >"$SANDBOX/metadata.json" 2>"$SANDBOX/metadata.err"); then
  say "$(tail -20 "$SANDBOX/metadata.err")"
  abstain "the sandbox workspace would not resolve — see the cargo output above"
fi
resolved=$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["packages"]))' "$SANDBOX/metadata.json" 2>/dev/null || echo "?")
say "resolved packages in the lifted closure: $resolved"

rule "3. the build, outside the monorepo"
# RUSTC_WRAPPER cleared: sccache is on by default on this host and would report
# a cold build as seconds of nothing, which would be a fake number.
t0=$(date +%s.%N)
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo build --offline -p commonwealth-rails --bin cw-rails 2>&1) \
  | tee "$SANDBOX/build.log" | grep -E "^(error|warning: unused)" >&2
build_rc=${PIPESTATUS[0]}
t1=$(date +%s.%N)
build_s=$(python3 -c "print(f'{$t1-$t0:.1f}')")
say "build: rc=$build_rc in ${build_s}s"
[ "$build_rc" = 0 ] || verdict 0 "the closure does not BUILD outside this monorepo (${build_s}s, see $SANDBOX/build.log)"
CW_RAILS="$SANDBOX/target/debug/cw-rails"
[ -x "$CW_RAILS" ] || verdict 0 "the build reported success and produced no cw-rails binary at $CW_RAILS"

rule "4. the package's own tests, in isolation"
# The half a build-only lift scores green: a leaf whose tests read the repo
# root compiles perfectly and fails here.
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo test --offline -p commonwealth-rails 2>&1) \
  | tee "$SANDBOX/test.log" | grep -E "^(error|test result|failures:)" >&2
test_rc=${PIPESTATUS[0]}
say "test: rc=$test_rc"
[ "$test_rc" = 0 ] || verdict 0 "the lifted package's own tests do not pass in isolation (see $SANDBOX/test.log)"

rule "5. the daemon joins a real mesh and serves the three routes"
# An invite is a MEMBER'S WORD, and this daemon cannot mint one — it has no
# `/internal/join`. Try the flag, then the env, then a full daemon on this host.
#
# THERE IS NO `mesh invite` VERB: the live invite is a FIELD on the mesh
# daemon's own status route (`join_link`), which is also where the desktop and
# the setup flow read it. Asking the route rather than a verb is what makes
# this work on a host where only the daemon is installed.
MESH_STATUS="${CW_RAILS_MESH_STATUS:-http://127.0.0.1:9741/v1/mesh/status}"
if [ -z "$INVITE" ]; then
  say "no --invite given; reading join_link from $MESH_STATUS"
  INVITE=$(curl -fsS "$MESH_STATUS" 2>"$SANDBOX/invite.err" | python3 -c '
import json,sys
try: print(json.load(sys.stdin).get("join_link") or "")
except Exception: pass
' 2>/dev/null)
fi
[ -n "$INVITE" ] || abstain "no invite, and $MESH_STATUS offered no join_link. Steps 1-4 MEASURED — the closure resolves, builds and passes its own tests outside the monorepo — and only the run abstains, because a rails daemon cannot found a mesh (it has no /internal/join, which is the scope that keeps it liftable). Pass --invite <link>, set CW_RAILS_INVITE, or start a mesh daemon on this host."
case "$INVITE" in
  *iroh=*|*dial=*) : ;;
  *) abstain "the invite carries no iroh dial string, and cw-rails joins over iroh only — a LAN/mDNS join means plaintext HTTP to an address, which is the posture this daemon exists not to have. Ask for an invite from a daemon with iroh on." ;;
esac
# An EXPIRED invite is an abstention, not a failure: the founder rejects it and
# the daemon would report a refusal that says nothing about whether the closure
# lifts. Named here, before the join, so the reason is the expiry rather than a
# 401 the reader has to interpret.
INVITE_EXP=$(printf '%s' "$INVITE" | sed -n 's/.*[?&]exp=\([0-9]*\).*/\1/p')
if [ -n "$INVITE_EXP" ] && [ "$INVITE_EXP" -lt "$(date +%s)" ] 2>/dev/null; then
  abstain "the invite expired at $(date -d "@$INVITE_EXP" 2>/dev/null || echo "$INVITE_EXP") and the founder is the authority on that — nothing was measured about the run. \`sovereign mesh rotate\` mints a fresh one, which then appears as join_link on $MESH_STATUS."
fi

DATA="$SANDBOX/data"
mkdir -p "$DATA" "$SANDBOX/library"
# A tiny file so a curl through the bridge has something real to fetch: a 200
# on an empty directory listing and a 200 on a byte range are different facts.
printf 'cw-rails-lift origin\n' > "$SANDBOX/library/index.html"
read -r ORIGIN_PORT LISTEN_PORT <<EOF
$(python3 - <<'PY'
import socket
def free():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p
print(free(), free())
PY
)
EOF
[ -n "$ORIGIN_PORT" ] && [ -n "$LISTEN_PORT" ] || abstain "could not find two free loopback ports"

# The media origin: a stand-in for the shim author's media server. Deliberately
# a plain HTTP server that authenticates NOTHING — which is precisely why the
# holder's admission decision has to be right, and why the daemon hands it the
# caller's verified identity on every request.
python3 -m http.server "$ORIGIN_PORT" --bind 127.0.0.1 --directory "$SANDBOX/library" \
  >"$SANDBOX/origin.log" 2>&1 &
ORIGIN_PID=$!
cat > "$DATA/rails.toml" <<EOF
name = "cw-rails-lift"
listen = $LISTEN_PORT

[media]
origin = "127.0.0.1:$ORIGIN_PORT"
EOF

say "joining with the invite (${#INVITE} chars)…"
(cd "$SANDBOX" && "$CW_RAILS" join "$INVITE" --data-dir "$DATA" --name cw-rails-lift 2>&1) \
  | tee "$SANDBOX/join.log" >&2
join_rc=${PIPESTATUS[0]}
case $join_rc in
  0) : ;;
  3) abstain "the join could not judge itself (rc 3) — see $SANDBOX/join.log" ;;
  *) verdict 0 "the lifted daemon did not join the mesh (rc $join_rc, see $SANDBOX/join.log)" ;;
esac
[ -f "$DATA/mesh.json" ] || verdict 0 "join reported success and wrote no mesh.json"

RUST_LOG="${RUST_LOG:-info}" "$CW_RAILS" run --data-dir "$DATA" >"$SANDBOX/daemon.log" 2>&1 &
DAEMON_PID=$!
API="http://127.0.0.1:$LISTEN_PORT"

# ≤60 s for a peer to appear. Bounded and polled — a sleep long enough to
# "usually" work is a flake with a schedule.
peer=""
for _ in $(seq 1 60); do
  kill -0 "$DAEMON_PID" 2>/dev/null || verdict 0 "the daemon exited while starting — see $SANDBOX/daemon.log"
  status=$(curl -fsS "$API/v1/mesh/status" 2>/dev/null)
  if [ -n "$status" ]; then
    peer=$(printf '%s' "$status" | python3 -c '
import json,sys
try: doc = json.load(sys.stdin)
except Exception: sys.exit(0)
others = [m for m in doc.get("members", []) if not m.get("is_self")]
print(others[0]["name"] if others else "")
' 2>/dev/null)
    [ -n "$peer" ] && break
  fi
  sleep 1
done
[ -n "$peer" ] || verdict 0 "no member other than this one appeared on /v1/mesh/status within 60s — the daemon joined and then never converged (see $SANDBOX/daemon.log)"
say "roster: this node plus at least '$peer'"

# The catalogue, POLLED on the same bound as the roster above.
#
# MEASURED 2026-09-11, on this instrument's first run: a single read here
# returned an empty list and the run reported a measured failure. The roster
# above had appeared instantly — it comes from the JOIN SNAPSHOT — while
# `capabilities.origins` rides a GOSSIP ROUND, so the two facts arrive
# seconds apart and the one-shot read was a race the instrument lost. The
# offer was on the roster by the next round.
first_offer() {
  curl -fsS "$1" 2>/dev/null | python3 -c '
import json,sys
try: doc = json.load(sys.stdin)
except Exception: sys.exit(0)
rows = doc.get("offering", [])
print(rows[0]["peer"] if rows else "")
' 2>/dev/null
}
offering=""
for _ in $(seq 1 60); do
  kill -0 "$DAEMON_PID" 2>/dev/null || verdict 0 "the daemon exited while converging the catalogue — see $SANDBOX/daemon.log"
  offering=$(first_offer "$API/v1/mesh/media")
  [ -n "$offering" ] && break
  sleep 1
done
if [ -z "$offering" ]; then
  # SECOND READING BEFORE A VERDICT. "Nobody offers a library on this mesh" and
  # "the catalogue did not converge" are different facts and an empty list does
  # not tell them apart (ARCH §18.3). The full daemon that issued the invite
  # serves the SAME route over its own roster; if it lists nobody but us, the
  # mesh genuinely has no library to reach and nothing was measured about this
  # daemon's catalogue.
  peer_offer=$(first_offer "${MESH_STATUS%/v1/mesh/status}/v1/mesh/media")
  if [ -z "$peer_offer" ] || [ "$peer_offer" = "cw-rails-lift" ]; then
    abstain "no member of this mesh other than this one advertises a media origin — the full daemon at ${MESH_STATUS%/v1/mesh/status} says the same, so there was no library to reach and the catalogue was not measured. A holder declares one with \`[iroh] media_origin\` and restarts."
  fi
  verdict 0 "GET /v1/mesh/media listed no offering member within 60s, while the full daemon lists '$peer_offer' — the catalogue rides gossiped capabilities.origins and nothing arrived (see $SANDBOX/daemon.log)"
fi
say "offering a library: $offering"

reach=$(curl -fsS "$API/v1/mesh/media?peer=$offering" 2>/dev/null)
url=$(printf '%s' "$reach" | python3 -c '
import json,sys
try: print(json.load(sys.stdin).get("url",""))
except Exception: pass
' 2>/dev/null)
case "$url" in
  http://127.0.0.1:*) : ;;
  "") verdict 0 "GET /v1/mesh/media?peer=$offering returned no URL: $reach" ;;
  *)  verdict 0 "the reach URL is not loopback ($url) — the media class was routed off iroh, which is a hole rather than a fallback" ;;
esac
say "viewer URL for $offering: $url"

# The last step, and the one that makes the URL a claim rather than a port: a
# real HTTP round trip through the bridge, by key, to the peer's own origin.
# ANY status is the pass — a 401 from a Jellyfin that wants a token is the
# plane working. What fails is no answer at all.
http_status=$(curl -s -o "$SANDBOX/through-bridge.out" -w '%{http_code}' --max-time 30 "$url/" 2>/dev/null)
case "$http_status" in
  000|"") verdict 0 "a GET through $url got no HTTP answer — the bridge accepted and the far end never sent a byte (the peer refuses this dial, or its origin is down)" ;;
  *) say "GET $url/ → HTTP $http_status ($(wc -c <"$SANDBOX/through-bridge.out") bytes)" ;;
esac

verdict 1 "built in ${build_s}s and RAN outside the monorepo: joined the mesh, converged a roster with '$peer', listed '$offering' as offering a library, and a GET through $url answered HTTP $http_status"
