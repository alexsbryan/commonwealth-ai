#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# cw-work-lift.sh — the instrument for the `cw-work-package-lift` bar
# (quality/campaigns/cw-lift.toml). The campaign's CLAIM block asks for TWO
# lifts and this is the second: `commonwealth-work` plus a package-only work
# peer BUILD and RUN outside this monorepo.
#
#   scripts/cw-work-lift.sh --sandbox [--image <ref>] [--dir <path>] [--keep]
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
#                               THE SHIPPED CAUSE, since the isolation floor
#                               reached the peer on 2026-09-10, is a missing
#                               BOUNDARY: the peer offers `process:v1` only when
#                               `Sandbox::probe` finds a rootless runtime and an
#                               image, so with no `--image` it publishes no offer
#                               and exits 3 naming that. Steps 1-4 still MEASURE
#                               — the closure resolves, builds and passes its
#                               own tests outside the monorepo — and only step 5
#                               abstains. Reporting that as `{"value": 0}` would
#                               say the lift failed when what happened is that a
#                               donor declined to run a stranger's argv without a
#                               boundary (ARCH §18.3). Give it an image this host
#                               already has and the donation measures again.
#                               (Until 2026-09-10 this said no build in the tree
#                               PROVIDED a container. `commonwealth-work`'s
#                               `sandbox` module now does, and it is the package's
#                               — so the abstention is a missing declaration, not
#                               a missing mechanism.)
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
# The image a donated unit runs INSIDE, on this host. The instrument does not
# choose one and ships none: `commonwealth-work::sandbox` documents that the
# image is the DONOR OPERATOR's declaration, and an instrument that hardcoded
# one would be making that choice on their behalf — and would report
# could-not-judge on every host that did not happen to have it. With none
# declared the peer publishes no offer and this run abstains, which is the
# honest reading of "this host has no boundary".
IMAGE="${CW_WORK_IMAGE:-}"

say() { printf '%s\n' "$*" >&2; }
rule() { say "── $* ─────────────────────────────────────────────" ; }

# The ONE place a verdict is emitted, so the artifact and the runner's value
# cannot disagree (ARCH §10.6). $1 value, $2 one-line reason.
verdict() {
  mkdir -p "$(dirname "$ARTIFACT")"
  # The image is in the row because a green with no boundary and a green
  # inside one are different facts, and a reader six weeks out cannot tell
  # them apart from a value alone.
  printf '{"value": %s, "reason": %s, "sandbox": "%s", "image": "%s", "at": "%s"}\n' \
    "$1" "$(printf '%s' "$2" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" \
    "$SANDBOX" "$IMAGE" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$ARTIFACT"
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
    if [ "$KEEP" = 1 ]; then say "sandbox kept at $SANDBOX (rail at ${RAIL:-unset})"
    else rm -rf "$SANDBOX" "${RAIL:-}" "${RAIL:-/nonexistent}.probe"; fi
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --sandbox) MODE=sandbox ;;
    --keep) KEEP=1 ;;
    --image) shift; IMAGE="${1:-}" ;;
    --dir) shift; SANDBOX="${1:-}" ;;
    -h|--help) sed -n '3,10p' "${BASH_SOURCE[0]}" >&2; exit 2 ;;
    *) say "unknown argument \`$1\`"; exit 2 ;;
  esac
  shift
done
[ "$MODE" = sandbox ] || { say "usage: scripts/cw-work-lift.sh --sandbox [--image <ref>] [--dir <path>] [--keep]"; exit 2; }

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

# THE RAIL IS A SIBLING OF THE WORKDIR, NOT A CHILD OF IT, and that is the
# property under test rather than a tidiness preference. The workdir is the
# ONE thing `Sandbox::command_line` mounts, so a rail under it would hand the
# peer's own `work` journal to every stranger's unit — while the demo went on
# claiming that a donor's state is unreachable by construction. The daemon
# gets this right for the same reason and by the same shape
# (`work_donor::resolve_workdir` hands out `donor_root/scratch`, a CHILD of the
# data dir, so the parent and the node key in it never cross), and an
# instrument that did it the other way would be proving a weaker thing than the
# one written down.
RAIL="${SANDBOX%/}.rail"
rm -rf "$RAIL" "$RAIL.probe"

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

rule "4b. the workdir a unit can actually build in"
# MEASURED 2026-09-10, on the first run of this instrument with an image: the
# shard unit failed inside the boundary with `no matching package named
# blake3`. Nothing was wrong with the plane. The boundary is `--network=none`
# with EXACTLY ONE mount, so a unit that compiles has no registry to resolve
# from and no writable `CARGO_HOME` — the donor's `~/.cargo` is on the other
# side of the mount by construction, which is the whole point of the mount
# rule.
#
# So the SUBMITTER ships the package cache in the workdir, and that is the
# general shape rather than an instrument trick: a `process:v1` unit that needs
# anything but the image gets it from the one directory that crosses. `cargo
# vendor` is 0.4 s and 109 MB here because it copies only this closure's
# dependencies, against the `Cargo.lock` that already travelled.
#
# This is what `--distribute` will have to do for D2, and it is the honest cost
# of the boundary: a CI shard's inputs are workdir contents, not host state.
if ! (cd "$SANDBOX" && RUSTC_WRAPPER= cargo vendor --offline --versioned-dirs vendor >/dev/null 2>"$SANDBOX/vendor.err"); then
  say "$(tail -5 "$SANDBOX/vendor.err")"
  abstain "the closure's dependencies could not be vendored into the workdir, so the shard unit could not have resolved inside a boundary — see $SANDBOX/vendor.err"
fi
mkdir -p "$SANDBOX/.cargo" "$SANDBOX/.cargo-home"
# Written into the SANDBOX, not copied from the repo. The lift refuses to carry
# this monorepo's `.cargo/config.toml` (see above) and that still holds: this
# file says only "resolve from the vendor directory beside you", which is a
# fact about the workdir a submitter prepared and not about the repository.
cat > "$SANDBOX/.cargo/config.toml" <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CFG
say "vendored $(find "$SANDBOX/vendor" -maxdepth 1 -mindepth 1 -type d | wc -l) crate source(s) into the workdir"

rule "5. the peer completes three heterogeneous units"
# The shard unit runs the SANDBOX's own tests as donated work, which is the
# demonstration: a lifted peer running a lifted crate's CI on the rail.
#
# `env CARGO_HOME=.cargo-home cargo …` and not a bare `cargo`, because a unit
# that compiles needs a WRITABLE package cache and the boundary discards every
# directory but this one. The path is RELATIVE on purpose: it is right as
# `/work/.cargo-home` inside the boundary and as `$SANDBOX/.cargo-home` without
# one, so nothing here has to know `sandbox`'s mount point. `env(1)` rather than
# an `sh -c` prologue keeps the unit's argv literally the program that runs.
#
# THE PROPER SEAM IS `ProcessPayload::env`, which exists and which the peer has
# no flag for. Adding one costs four lines and the peer is at 400 of its 400-line
# cap (`cw-work-second-lift`), so this is a deliberate deferral to a target that
# is the operator's to move, not a preference.
#
# And a live seam neither can close: `host_satisfies` checks
# `Precondition::Binary` against the DONOR'S HOST while the unit runs inside the
# DONOR'S IMAGE. Under a container boundary that is the wrong subject in both
# directions — a host without python3 refuses a unit its image could have run,
# and a host with cargo accepts one its image cannot. Which is also why argv[0]
# being `env` here costs less than it looks: the precondition was already about
# the wrong machine.
(cd "$SANDBOX" && "$SANDBOX/target/debug/examples/work_peer" \
    --root "$RAIL" --workdir "$SANDBOX" --label "cw-work-lift peer" \
    --image "$IMAGE" \
    -- env CARGO_HOME=.cargo-home cargo test --offline -q -p commonwealth-rail-core \
    2>&1) | tee "$SANDBOX/peer.log" >&2
peer_rc=${PIPESTATUS[0]}
say "peer: rc=$peer_rc"
case $peer_rc in
  0) : ;;
  3) abstain "the peer could not judge its own run — see $SANDBOX/peer.log" ;;
  *) verdict 0 "the peer did not complete its three units (rc $peer_rc, see $SANDBOX/peer.log)" ;;
esac

rule "6. and the boundary HOLDS — the ledger's own watched red"
# `sovereign/DEFAULTS_LEDGER.md`'s `process:v1` row names the falsification for
# the whole mechanism: "a unit that tries to read `node_key` or open a socket
# and FAILS". Until 2026-09-10 that was a sentence. It is this step.
#
# Two escapes, attempted by a real donated unit through the real executor —
# never by a podman line written here, which would be a second implementation
# of the boundary and could pass while the shipped one leaked (§10.6).
cat > "$SANDBOX/probe.sh" <<EOF
# Exits 0 only if BOTH escapes were refused. Run from the unit's workdir.
if ! command -v python3 >/dev/null 2>&1; then
  echo "PROBE BROKEN: no python3, so a blocked socket cannot be told from a missing interpreter"; exit 1
fi
if python3 -c 'import socket; socket.setdefaulttimeout(4); socket.create_connection(("1.1.1.1", 53))' 2>/dev/null; then
  echo "ESCAPE: this unit opened a TCP connection to the internet"; exit 1
fi
echo "ok: no network from inside the unit"
if [ -e "$RAIL" ]; then
  echo "ESCAPE: this unit can see the donor's rail — and a rail holds a donor's identity"; exit 1
fi
echo "ok: the donor's rail is not on this filesystem"
EOF

# THE CONTROL COMES FIRST. On this host, with no boundary at all, the probe MUST
# fail — otherwise it is a check with no failing input and its green means
# nothing (§18.1). Watched failing on 2026-09-10: the socket branch on a machine
# with a route, and the rail branch on its own with the network down.
if (cd "$SANDBOX" && sh probe.sh >"$SANDBOX/probe-control.log" 2>&1); then
  say "$(cat "$SANDBOX/probe-control.log")"
  abstain "the escape probe PASSED on the bare host, so it cannot detect an escape — nothing was measured about the boundary"
fi
say "control: the probe fails without a boundary — $(head -1 "$SANDBOX/probe-control.log")"

# A SECOND RAIL, because a peer handed the first one finds every unit already
# complete and would report a cheerful green having run nothing.
(cd "$SANDBOX" && "$SANDBOX/target/debug/examples/work_peer" \
    --root "$RAIL.probe" --workdir "$SANDBOX" --label "cw-work-lift probe" \
    --image "$IMAGE" -- sh probe.sh 2>&1) | tee "$SANDBOX/probe.log" >&2
probe_rc=${PIPESTATUS[0]}
say "probe: rc=$probe_rc"
case $probe_rc in
  0) : ;;
  3) abstain "the probe run could not judge itself — see $SANDBOX/probe.log" ;;
  # The probe's own stdout is not carried back: its unit is `ExitCodeOnly`, which
  # is the point of that unit and not worth bending for a diagnostic. The reason
  # therefore names the command that shows which escape happened.
  *) verdict 0 "a donated unit ESCAPED the boundary (rc $probe_rc) — reproduce with: cd $SANDBOX && sh probe.sh, then the same argv through the peer" ;;
esac

acts=$(grep -c '"op"' "$RAIL/rings/work/ring_oplog.jsonl" 2>/dev/null || echo 0)
# The peer decides its own verdict, so this is the guard on the DECIDER: a
# green report with an empty journal would mean the fold read nothing and
# said so cheerfully. A submit, an offer and three lease/report pairs is 8.
[ "$acts" -ge 8 ] || verdict 0 "the peer reported green with only $acts act(s) on the work rail — a submit, an offer and three lease/report pairs is 8"

verdict 1 "built in ${build_s}s, ran three heterogeneous units on a $acts-act rail and refused both escapes, outside the monorepo, inside \`$IMAGE\`"
