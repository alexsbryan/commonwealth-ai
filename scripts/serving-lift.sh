#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# serving-lift.sh — the instrument for the serving package's physical lift.
# The package is `[[package]] name = "serving"` in quality/ARCH_LAYERS.toml and
# the design is `sovereign/SERVING_BOUNDARY.md`, "What is enforced, and what is
# not" Tier 2. The campaign rung that names this verdict is `dm-mesh-serving`
# (quality/campaigns/domains.toml), whose done condition is "serving-lift
# verdict 1"; the `[[bar]]` row that measures it is owed by that rung.
#
#   scripts/serving-lift.sh --sandbox [--dir <path>] [--keep]
#
# THE BAR IS A PHYSICAL LIFT, NOT A CRATE-NAME COUNT (the lesson
# scripts/cw-work-lift.sh records). `boundary-gate` reads manifests and cannot
# see a hand-spelled relative path, a `build.rs`, an `include_str!` that escapes
# the crate root, or a test that reads the repo root through
# `CARGO_MANIFEST_DIR.parent()`. So this copies the package's closure to a
# scratch directory OUTSIDE the repository, synthesises a root workspace there,
# and builds and tests it with nothing of this monorepo on the path.
#
# WHICH CRATES THE PACKAGE HOLDS IS READ, NEVER RESTATED. The set lives in
# `[[package]] name = "serving"` in quality/ARCH_LAYERS.toml, for the reason
# that file's head gives: a second copy of a list drifts while the gate stays
# green (ARCH §8, §9).
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
#                               unresolvable), OR a later step abstained by name
#                               because the package is not yet extracted.
#                               Nothing was measured by the abstaining step.
#   rc 2                        usage
#   rc 127                      instrument-missing (the runner's own reading
#                               when this file does not exist)
#
# The 0 and the 3 are the whole point. A 0 written by a missing tool would be a
# substitution indistinguishable from a measured failure, which is exactly what
# the bar's own text forbids. Every abstention below names what was absent, on
# stderr, before it exits 3.
#
# STEPS 5-8 ABSTAIN BY NAME, and steps 1-4 measure for real. The package is
# declared red and not yet extracted: today step 1 fails because the peg
# `sovereign-serving` still spells its `commonwealth-{core,state}` deps as
# relative paths, so the closure cannot leave the monorepo (SERVING_BOUNDARY.md
# "The two tiers"; rung domains-10 empties the peg). Once steps 1-4 pass, the
# RUN half — stub OpenAI endpoints, admission 429 with `Retry-After`, replay
# reproducing every decision, the positive/negative controls and the decider
# guard — abstains by name rather than printing a pass it did not earn. One
# honesty note the replay step must carry when it lands: `replay_decision`
# assumes `RankObjective::Product` (scheduler_core.rs:72-76), so it abstains
# rather than pass on a record whose objective it cannot read.
#
# NOT `set -e`: a failure here is a VERDICT to classify, never an abort.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT="$REPO/target/serving-lift/last.json"
SANDBOX=""
KEEP=0
MODE=""

say() { printf '%s\n' "$*" >&2; }
rule() { say "── $* ─────────────────────────────────────────────" ; }

# The ONE place a verdict is emitted, so the artifact and the runner's value
# cannot disagree (ARCH §8). $1 value, $2 one-line reason.
verdict() {
  mkdir -p "$(dirname "$ARTIFACT")"
  printf '{"value": %s, "reason": %s, "at": "%s"}\n' \
    "$1" "$(printf '%s' "$2" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')" \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$ARTIFACT"
  say ""
  say "VERDICT $1 — $2"
  printf '{"value": %s, "artifact": "target/serving-lift/last.json"}\n' "$1"
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
    -h|--help) say "usage: scripts/serving-lift.sh --sandbox [--dir <path>] [--keep]"; exit 2 ;;
    *) say "unknown argument \`$1\`"; exit 2 ;;
  esac
  shift
done
[ "$MODE" = sandbox ] || { say "usage: scripts/serving-lift.sh --sandbox [--dir <path>] [--keep]"; exit 2; }

# ── Preconditions of the RUN, not of the claim ─────────────────────────────
command -v cargo   >/dev/null 2>&1 || abstain "cargo is not on PATH — nothing can be built, so nothing was measured"
command -v python3 >/dev/null 2>&1 || abstain "python3 is not on PATH — the lift's own closure planner needs it"
[ -f "$REPO/quality/ARCH_LAYERS.toml" ] || \
  abstain "quality/ARCH_LAYERS.toml is not in this checkout at $REPO — this is not the tree the bar is about"

# The sandbox lives OUTSIDE the repository, and that is load-bearing twice
# over: cargo inherits `.cargo/config.toml` from ANY ancestor directory, and a
# sandbox under the repo would silently pick up this workspace's linker and
# rustflags — the exact monorepo dependency the lift exists to disprove.
[ -n "$SANDBOX" ] || SANDBOX="${TMPDIR:-/tmp}/serving-lift.$$"
case "$SANDBOX" in "$REPO"|"$REPO"/*) say "refusing a sandbox inside the repository: $SANDBOX"; exit 2 ;; esac
rm -rf "$SANDBOX"; mkdir -p "$SANDBOX" || abstain "could not create the sandbox at $SANDBOX"

rule "1. the closure, copied outside the monorepo"
python3 - "$REPO" "$SANDBOX" <<'PY'
import os, re, shutil, sys, tomllib
repo, sandbox = sys.argv[1], sys.argv[2]
root = tomllib.loads(open(os.path.join(repo, "Cargo.toml")).read())
ws = root["workspace"]
wsdeps = ws.get("dependencies", {})

# THE PACKAGE'S CRATE SET LIVES IN EXACTLY ONE PLACE. Read it from the registry
# rather than restating it (ARCH §8, §9); a second copy of the list would drift
# while boundary-gate stayed green.
layers = tomllib.loads(open(os.path.join(repo, "quality/ARCH_LAYERS.toml")).read())
pkgs = [p for p in layers.get("package", []) if p.get("name") == "serving"]
if len(pkgs) != 1:
    print(f'PACKAGE quality/ARCH_LAYERS.toml has {len(pkgs)} [[package]] name="serving" rows, want 1')
    sys.exit(4)
seeds = pkgs[0].get("crates", [])
if not seeds:
    print('PACKAGE [[package]] name="serving" lists no crates')
    sys.exit(4)

# crate name -> directory, from the workspace member list.
dirs = {}
for m in ws.get("members", []):
    mp = os.path.join(repo, m, "Cargo.toml")
    if not os.path.exists(mp):
        continue
    try:
        dirs[tomllib.loads(open(mp).read())["package"]["name"]] = os.path.join(repo, m)
    except Exception:
        continue
missing = [c for c in seeds if c not in dirs]
if missing:
    print("MISSING " + " ".join(missing) + " (a package crate is not a workspace member)")
    sys.exit(4)

def manifest(path): return tomllib.loads(open(path).read())
def tables(m):
    for t in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name, spec in m.get(t, {}).items():
            yield t, name, (spec if isinstance(spec, dict) else {"version": spec})

# Walk the workspace-local closure from the package's own crates,
# dev-dependencies included: the rules count them, because a third party who
# lifts the package carries its tests.
found, hand_paths, referenced, queue = {}, [], set(), [(c, dirs[c]) for c in seeds]
while queue:
    name, cdir = queue.pop()
    if name in found:
        continue
    found[name] = cdir
    for table, dep, spec in tables(manifest(os.path.join(cdir, "Cargo.toml"))):
        if spec.get("workspace"):
            referenced.add(dep)
            entry = wsdeps.get(dep)
            if entry is None:
                print(f"UNDECLARED {name} -> {dep}: `workspace = true` with no root entry")
                sys.exit(4)
            if isinstance(entry, dict) and "path" in entry:
                queue.append((dep, os.path.join(repo, entry["path"])))
        elif "path" in spec:
            # A hand-spelled relative path hardcodes this monorepo's directory
            # depth and dies at `cargo metadata` outside it, before a line
            # compiles. boundary-gate validates that the EDGE is legal, never
            # how it is SPELLED.
            hand_paths.append(f"{name} -> {dep} = {{ path = \"{spec['path']}\" }} ({table})")

if hand_paths:
    for h in hand_paths:
        print("HAND-SPELLED-PATH " + h)
    sys.exit(4)

# Hazards a green `cargo tree` and a green boundary-gate both miss. Reported
# whether or not they bite, because the build below is what decides and a
# reader wants to know what was in the sandbox either way. `include_str!` with
# a sibling path is crate-local and legal; only a path that ESCAPES the crate
# root is what SERVING_BOUNDARY.md's lift exists to catch.
hazard = re.compile(r'CARGO_MANIFEST_DIR|git\s+ls-files|include_(str|bytes)!\s*\(\s*"[^"]*\.\.')
for name, cdir in sorted(found.items()):
    if os.path.exists(os.path.join(cdir, "build.rs")):
        print(f"HAZARD {name}: build.rs (a package crate carries no build script)")
    for dirpath, dirnames, files in os.walk(cdir):
        dirnames[:] = [d for d in dirnames if d not in ("target", ".git")]
        for f in files:
            if not f.endswith(".rs"):
                continue
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
for k, v in ws.get("package", {}).items():
    out.append(f"{k} = {render(v)}")
out.append("")
for section, body in ws.get("lints", {}).items():
    out.append(f"[workspace.lints.{section}]")
    for k, v in body.items():
        out.append(f"{k} = {render(v)}")
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
print("PACKAGE " + " ".join(seeds))
print("CRATES " + " ".join(sorted(found)))
print(f"COUNT {len(found)}")
PY
rc=$?
case $rc in
  0) : ;;
  4) verdict 0 "the closure cannot leave the monorepo — see the MISSING / HAND-SPELLED-PATH / UNDECLARED line above" ;;
  *) abstain "the closure planner failed with rc $rc before anything was copied" ;;
esac

# NOT copied, deliberately, and each absence is a claim being tested:
#   .cargo/config.toml  a third party has their own linker settings
#   clippy.toml         the workspace's own lint policy is not the package's
#   rust-toolchain.toml the lift must build on a stock toolchain
say "toolchain in the sandbox: $(cd "$SANDBOX" && cargo --version 2>&1 | head -1)"
say "sandbox: $SANDBOX  (repo is $REPO — nothing under it is on this path)"

# Resolution BEFORE the build, and its failure is an abstention rather than a
# verdict: a registry that cannot be reached says nothing about whether this
# closure lifts (ARCH §18.2). A build failure AFTER a clean resolve does.
if ! (cd "$SANDBOX" && RUSTC_WRAPPER= cargo metadata --format-version 1 >/dev/null 2>"$SANDBOX/metadata.err"); then
  say "$(tail -20 "$SANDBOX/metadata.err")"
  abstain "the sandbox workspace would not resolve — see the cargo output above"
fi

rule "2. the build, outside the monorepo"
# RUSTC_WRAPPER cleared: sccache is on by default on this host and would report
# a cold build as time spent in nothing.
t0=$(date +%s.%N)
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo build --workspace 2>&1) \
  | tee "$SANDBOX/build.log" | grep -E "^(error|warning: unused)" >&2
build_rc=${PIPESTATUS[0]}
t1=$(date +%s.%N)
build_s=$(python3 -c "print(f'{$t1-$t0:.1f}')")
say "build: rc=$build_rc in ${build_s}s"
[ "$build_rc" = 0 ] || verdict 0 "the closure does not BUILD outside this monorepo (${build_s}s, see $SANDBOX/build.log)"

rule "3. the package's own tests, in isolation"
# A build-only lift scores a leaf's test that reads the repo root green.
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo test --workspace 2>&1) \
  | tee "$SANDBOX/test.log" | grep -E "^(error|test result|failures:)" >&2
test_rc=${PIPESTATUS[0]}
say "test: rc=$test_rc"
[ "$test_rc" = 0 ] || verdict 0 "the lifted package's own tests do not pass in isolation (see $SANDBOX/test.log)"

rule "4. no inference backend in the closure"
# `SERVING_BOUNDARY.md` "The two tiers": the package's local engine
# (`sovereign-compute`) and the inference stack are deliberately outside this
# closure. A lifted package that reaches a native backend is not liftable on a
# stock toolchain, and `cargo tree -i` is what says so.
for backend in llama-cpp-4 ort iroh; do
  if (cd "$SANDBOX" && RUSTC_WRAPPER= cargo tree -i "$backend" >/dev/null 2>&1); then
    verdict 0 "the closure reaches \`$backend\` — the package is not liftable without the inference stack"
  fi
done
say "llama-cpp-4, ort and iroh are all absent from the closure"

# ── The RUN half. The package is declared red and not yet extracted, so each
# ── step below abstains by name rather than printing a pass it did not earn.
rule "5. the package RUNS against N stub OpenAI endpoints"
abstain "step 5 (run against N stub OpenAI endpoints): package not yet extracted"

rule "6. admission answers 429 with Retry-After"
abstain "step 6 (admission 429 with Retry-After): package not yet extracted"

rule "7. replay reproduces every decision"
abstain "step 7 (replay reproduces every decision): package not yet extracted"

rule "8. positive and negative control, and the decider guard"
abstain "step 8 (positive + negative control + decider guard): package not yet extracted"

verdict 1 "the serving package resolved, built and passed its own tests outside the monorepo with no inference backend in its closure"
