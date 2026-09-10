#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-work-lift.sh — the instrument for the `cw-work-package-lift` bar
# (quality/campaigns/cw-lift.toml). The campaign's CLAIM block asks for TWO
# lifts and this is the second: `commonwealth-work` plus a package-only work
# peer BUILD and RUN outside this monorepo.
#
#   scripts/cw-work-lift.sh --sandbox [--dir <path>] [--keep]
#
# THE BAR IS A PHYSICAL LIFT, NOT A CRATE-NAME COUNT. `cargo tree` cannot see
# a `build.rs`, an `include_str!` that escapes the crate root, a hand-spelled
# relative path dependency, or a test that reads the repo root through
# CARGO_MANIFEST_DIR.parent() — and each of those is a lift that fails on a
# closure the gate scores green. So this copies the closure to a scratch
# directory OUTSIDE the repository, synthesises a root workspace there, and
# builds and tests and RUNS it with nothing of this monorepo on the path.
#
# ── The four verdicts, and how the runner reads them (ARCH §18.2, §18.3) ────
#
# `scripts/co-lineage.py::measure_bar` maps this script's exit code and last
# stdout line onto a measurement row, so the mapping is a contract:
#
#   rc 0, stdout {"value": 1}   the lift worked — passed
#   rc 0, stdout {"value": 0}   MEASURED FAILURE — the closure did not lift
#   rc 3                        could-not-judge: a precondition of the RUN is
#                               absent (no cargo, no python3, dependencies
#                               unresolvable). Nothing was measured.
#   rc 2                        usage
#   rc 127                      instrument-missing (the runner's own reading
#                               when this file does not exist)
#
# The 0 and the 3 are the whole point of writing this carefully. A 0 written
# by a missing tool would be a substitution indistinguishable from a measured
# failure, which is exactly what the bar's own text forbids. Every abstention
# below names what was absent, on stderr, before it exits 3.
#
# NOT `set -e`: a failure here is a VERDICT to classify, never an abort.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT="$REPO/target/cw-work-lift/last.json"
SANDBOX=""
KEEP=0
MODE=""

say() { printf '%s\n' "$*" >&2; }
rule() { say "── $* ─────────────────────────────────────────────" ; }

# The ONE place a verdict is emitted, so the artifact and the runner's value
# cannot disagree (ARCH §10.6). $1 value, $2 one-line reason.
verdict() {
  mkdir -p "$(dirname "$ARTIFACT")"
  printf '{"value": %s, "reason": %s, "sandbox": "%s", "at": "%s"}\n' \
    "$1" "$(printf '%s' "$2" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" \
    "$SANDBOX" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$ARTIFACT"
  say ""
  say "VERDICT $1 — $2"
  printf '{"value": %s, "artifact": "target/cw-work-lift/last.json"}\n' "$1"
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

cleanup() {
  if [ -n "$SANDBOX" ] && [ -d "$SANDBOX" ]; then
    if [ "$KEEP" = 1 ]; then say "sandbox kept at $SANDBOX"
    else rm -rf "$SANDBOX"; fi
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --sandbox) MODE=sandbox ;;
    --keep) KEEP=1 ;;
    --dir) shift; SANDBOX="${1:-}" ;;
    -h|--help) sed -n '3,10p' "${BASH_SOURCE[0]}" >&2; exit 2 ;;
    *) say "unknown argument \`$1\`"; exit 2 ;;
  esac
  shift
done
[ "$MODE" = sandbox ] || { say "usage: scripts/cw-work-lift.sh --sandbox [--dir <path>] [--keep]"; exit 2; }

# ── Preconditions of the RUN, not of the claim ─────────────────────────────
command -v cargo   >/dev/null 2>&1 || abstain "cargo is not on PATH — nothing can be built, so nothing was measured"
command -v python3 >/dev/null 2>&1 || abstain "python3 is not on PATH — the lift's own planner and one of the peer's three units need it"
[ -f "$REPO/commonwealth/crates/commonwealth-work/Cargo.toml" ] || \
  abstain "commonwealth-work is not in this checkout at $REPO — this is not the tree the bar is about"

# The sandbox lives OUTSIDE the repository, and that is load-bearing twice
# over: cargo inherits `.cargo/config.toml` from ANY ancestor directory, and a
# sandbox under the repo would silently pick up this workspace's linker and
# rustflags — the exact monorepo dependency the lift exists to disprove.
[ -n "$SANDBOX" ] || SANDBOX="${TMPDIR:-/tmp}/cw-work-lift.$$"
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
# this reads what was actually copied — one of the two can be stale.
FORBIDDEN = re.compile(r"^(sovereign-|corpus-engine|commonwealth-(knowledge|inference|api)$)")

def manifest(path): return tomllib.loads(open(path).read())
def tables(m):
    for t in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name, spec in m.get(t, {}).items():
            yield t, name, (spec if isinstance(spec, dict) else {"version": spec})

# Walk the workspace-local closure from commonwealth-work, dev-dependencies
# included: BOUNDARY.md's rules count them, because a third party who lifts the
# package carries its tests.
seeds = {"commonwealth-work": os.path.join(repo, "commonwealth/crates/commonwealth-work")}
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
            # The 1f' finding, in TOML: a hand-spelled relative path hardcodes
            # this monorepo's directory depth and dies at `cargo metadata`,
            # before a line compiles. boundary-gate is blind to it — it
            # validates that the EDGE is legal, never how it is SPELLED.
            hand_paths.append(f"{name} -> {dep} = {{ path = \"{spec['path']}\" }} ({table})")

if hand_paths:
    for h in hand_paths: print("HAND-SPELLED-PATH " + h)
    sys.exit(4)

# Hazards that a green `cargo tree` and a green boundary-gate both miss.
# Reported whether or not they bite, because the build below is what decides
# and a reader wants to know what was in the sandbox either way.
# `include_str!("sibling.toml")` is crate-local and legal; only a path that
# ESCAPES the crate root is what BOUNDARY.md §4 forbids, so the pattern says
# so rather than reporting every embed and teaching the reader to skim past.
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
# shape. Studio's lift had to preserve it and BOUNDARY.md records that as the
# smell it is: a layout-preserving sandbox proves the crates compile where they
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
if ! (cd "$SANDBOX" && RUSTC_WRAPPER= cargo metadata --format-version 1 >/dev/null 2>"$SANDBOX/metadata.err"); then
  say "$(tail -20 "$SANDBOX/metadata.err")"
  abstain "the sandbox workspace would not resolve — see the cargo output above"
fi

rule "3. the build, outside the monorepo"
# RUSTC_WRAPPER cleared: sccache is on by default on this host and reported the
# 1f' lift's 8.4s cold build as 12.4s of nothing, which would be a fake number.
t0=$(date +%s.%N)
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo build -p commonwealth-work --features process --example work_peer 2>&1) \
  | tee "$SANDBOX/build.log" | grep -E "^(error|warning: unused)" >&2
build_rc=${PIPESTATUS[0]}
t1=$(date +%s.%N)
build_s=$(python3 -c "print(f'{$t1-$t0:.1f}')")
say "build: rc=$build_rc in ${build_s}s"
[ "$build_rc" = 0 ] || verdict 0 "the closure does not BUILD outside this monorepo (${build_s}s, see $SANDBOX/build.log)"

rule "4. the package's own tests, in isolation"
# The half that caught the 1f' failure: `490 passed, 2 failed`, both of them in
# a LEAF's tests reading the repo root. A build-only lift scores that green.
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo test --workspace --features commonwealth-work/process 2>&1) \
  | tee "$SANDBOX/test.log" | grep -E "^(error|test result|failures:)" >&2
test_rc=${PIPESTATUS[0]}
say "test: rc=$test_rc"
[ "$test_rc" = 0 ] || verdict 0 "the lifted package's own tests do not pass in isolation (see $SANDBOX/test.log)"

rule "5. the peer completes three heterogeneous units"
# The shard unit runs the SANDBOX's own tests as donated work, which is the
# demonstration: a lifted peer running a lifted crate's CI on the rail.
(cd "$SANDBOX" && "$SANDBOX/target/debug/examples/work_peer" \
    --root "$SANDBOX/.work-rail" --workdir "$SANDBOX" --label "cw-work-lift peer" \
    -- cargo test --offline -q -p commonwealth-rail-core 2>&1) | tee "$SANDBOX/peer.log" >&2
peer_rc=${PIPESTATUS[0]}
say "peer: rc=$peer_rc"
case $peer_rc in
  0) : ;;
  3) abstain "the peer could not judge its own run — see $SANDBOX/peer.log" ;;
  *) verdict 0 "the peer did not complete its three units (rc $peer_rc, see $SANDBOX/peer.log)" ;;
esac

acts=$(grep -c '"op"' "$SANDBOX/.work-rail/rings/work/ring_oplog.jsonl" 2>/dev/null || echo 0)
# The peer decides its own verdict, so this is the guard on the DECIDER: a
# green report with an empty journal would mean the fold read nothing and
# said so cheerfully. A submit, an offer and three lease/report pairs is 8.
[ "$acts" -ge 8 ] || verdict 0 "the peer reported green with only $acts act(s) on the work rail — a submit, an offer and three lease/report pairs is 8"

verdict 1 "built in ${build_s}s and ran three heterogeneous units on a $acts-act rail, outside the monorepo"
