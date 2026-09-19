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
# STEPS 1-4 MEASURE, AND SO DO 5-8. Steps 1-4 prove the closure RESOLVES,
# BUILDS, TESTS and carries no inference backend outside the monorepo. Steps
# 5-8 run the package's OWN harness
# (`sovereign-serving-host/tests/main/serving_lift_harness.rs`, run inside the
# sandbox so nothing of this monorepo is on the path): it stands up N stub
# OpenAI endpoints, routes K requests across them, meets a `429` +
# `Retry-After` on the K+1th, replays every decision, and guards the decider
# with positive and negative controls. Each step below reads one `LIFT `
# evidence line the harness printed; the harness asserts the same facts, so a
# green harness and a green lift cannot disagree.
#
# The `429` is the STUB endpoint's refusal, not the host admission's:
# `admission::shed_response` renders `503 + Retry-After` (this crate's own
# contract, asserted in `admission.rs`), and `decision_log::looks_shed`
# (decision_log.rs:907) reads both. The host's `503` shed is the harness's
# negative control.
#
# One honesty note the replay step carries: `replay_decision` assumes
# `RankObjective::Product` (scheduler_core.rs:72-76), which is the only
# objective production ranks on (`peer_inference.rs:1775`), so a record whose
# objective it cannot read is skipped by name, never passed.
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

# ── The toolchain precondition, checked BEFORE the build ────────────────────
# Two facts, one decider:
#  * The sandbox inherits `[workspace.package] rust-version` from the copied
#    root manifest and does NOT carry rust-toolchain.toml (above), so it builds
#    on whatever toolchain is selected here. Older than the declared MSRV,
#    cargo refuses the build and step 2 would write a MEASURED 0 the instrument
#    cannot measure (measured 2026-09-15: host default 1.94.1 against 1.95).
#  * The package's own tests include compiler-version-specific trybuild
#    `.stderr` snapshots, so a satisfying-but-different toolchain fails step 3
#    on diagnostics alone (measured 2026-09-15: `nightly-2026-07-01` fails
#    `kernel-types --test answer_reds`; the pinned `1.95.0` passes).
# So: prefer the toolchain the workspace PINS (`rust-toolchain.toml`), then the
# first installed one satisfying the MSRV, then the ambient one; abstain by
# name only when none satisfies the MSRV.
MSRV=$(python3 -c "import tomllib;print(tomllib.load(open('$REPO/Cargo.toml','rb'))['workspace']['package'].get('rust-version',''))" 2>/dev/null)
SANDBOX_TOOLCHAIN=""
if [ -n "$MSRV" ]; then
  ambient=$(cd "$SANDBOX" && rustc --version 2>/dev/null | sed -E 's/^rustc ([0-9]+\.[0-9]+\.[0-9]+).*/\1/')
  # version_lt A B — true when A is strictly older than B.
  version_lt() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$1" ] && [ "$1" != "$2" ]; }
  if command -v rustup >/dev/null 2>&1; then
    pinned=$(python3 -c "import tomllib;print(tomllib.load(open('$REPO/rust-toolchain.toml','rb')).get('toolchain',{}).get('channel',''))" 2>/dev/null)
    for tc in "$pinned" $(rustup toolchain list 2>/dev/null | awk '{print $1}'); do
      [ -n "$tc" ] || continue
      v=$(rustup run "$tc" rustc --version 2>/dev/null | sed -E 's/^rustc ([0-9]+\.[0-9]+\.[0-9]+).*/\1/')
      if [ -n "$v" ] && ! version_lt "$v" "$MSRV"; then SANDBOX_TOOLCHAIN="$tc"; break; fi
    done
  fi
  if [ -n "$SANDBOX_TOOLCHAIN" ]; then
    export RUSTUP_TOOLCHAIN="$SANDBOX_TOOLCHAIN"
    say "MSRV $MSRV: sandbox runs on \`$SANDBOX_TOOLCHAIN\` (ambient rustc ${ambient:-unknown})"
  elif [ -n "$ambient" ] && ! version_lt "$ambient" "$MSRV"; then
    say "MSRV $MSRV: sandbox runs on the ambient rustc $ambient (no pinned or installed toolchain to select)"
  else
    abstain "the workspace declares rust-version = $MSRV and no installed toolchain satisfies it — nothing was measured (install a rustc >= $MSRV, or run on a host whose default does)"
  fi
fi

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

# ── The RUN half. The package's own harness
# ── (`tests/main/serving_lift_harness.rs`) runs INSIDE the sandbox, so nothing
# ── of this monorepo is on the path, and prints the `LIFT ` evidence each step
# ── below reads.
rule "5. the package RUNS against N stub OpenAI endpoints"
(cd "$SANDBOX" && RUSTC_WRAPPER= cargo test -p sovereign-serving-host --test main \
    -- --nocapture serving_lift_harness 2>&1) \
  | tee "$SANDBOX/run.log" | grep -E '^LIFT ' >&2
run_rc=${PIPESTATUS[0]}
say "harness: rc=$run_rc"
[ "$run_rc" = 0 ] || verdict 0 "the package's own run harness did not pass in isolation (rc $run_rc, see $SANDBOX/run.log)"
run_log="$SANDBOX/run.log"
endpoints=$(sed -nE 's/^LIFT endpoints=([0-9]+).*/\1/p' "$run_log" | head -1)
requests=$(sed -nE 's/^LIFT endpoints=[0-9]+ requests=([0-9]+).*/\1/p' "$run_log" | head -1)
served=$(sed -nE 's/^LIFT endpoints=[0-9]+ requests=[0-9]+ served=([0-9]+).*/\1/p' "$run_log" | head -1)
[ -n "$endpoints" ] && [ "$endpoints" -ge 1 ] || verdict 0 "the run harness reported no stub endpoints (see $run_log)"
[ -n "$requests" ] && [ "$requests" -ge 1 ] || verdict 0 "the run harness reported no requests (see $run_log)"
[ -n "$served" ] && [ "$served" -ge "$requests" ] || verdict 0 "the stub pool served $served of $requests request(s) (see $run_log)"
say "step 5: $served/$requests request(s) served across $endpoints stub endpoint(s)"

rule "6. the K+1th request meets a 429 with Retry-After"
# The 429 is the STUB endpoint's refusal; the host's own `shed_response`
# renders 503 + Retry-After and is the negative control in step 8. The harness
# asserts the refusal is recorded as a SHED, never a fault.
shed_count=$(sed -nE 's/^LIFT shed=429 retry_after=[0-9]+ count=([0-9]+).*/\1/p' "$run_log" | head -1)
[ -n "$shed_count" ] && [ "$shed_count" -ge 1 ] || verdict 0 "the run harness saw no 429 refusal (see $run_log)"
say "step 6: $shed_count shed(s) recorded, each 429 + Retry-After"

rule "7. replay reproduces every decision"
# Honesty note: `replay_decision` assumes `RankObjective::Product`
# (scheduler_core.rs:72-76), the only objective production ranks on
# (peer_inference.rs:1775); a record whose objective it cannot read is skipped
# by name, never passed. The harness asserts 1.0 policy AND scorer agreement.
replayed=$(sed -nE 's/^LIFT replay=([0-9]+)\/[0-9]+ scorer=.*/\1/p' "$run_log" | head -1)
replayable=$(sed -nE 's/^LIFT replay=[0-9]+\/([0-9]+) scorer=.*/\1/p' "$run_log" | head -1)
scorer_agreed=$(sed -nE 's/^LIFT replay=[0-9]+\/[0-9]+ scorer=([0-9]+)\/[0-9]+.*/\1/p' "$run_log" | head -1)
scorer_checked=$(sed -nE 's/^LIFT replay=[0-9]+\/[0-9]+ scorer=[0-9]+\/([0-9]+).*/\1/p' "$run_log" | head -1)
[ -n "$replayed" ] && [ "$replayed" -ge 1 ] || verdict 0 "the run harness reported no replay (see $run_log)"
[ "$replayed" = "$replayable" ] || verdict 0 "replay reproduced $replayed of $replayable decisions (see $run_log)"
[ "$scorer_agreed" = "$scorer_checked" ] || verdict 0 "scorer replay agreed on $scorer_agreed of $scorer_checked candidates (see $run_log)"
say "step 7: $replayed/$replayable decisions and $scorer_agreed/$scorer_checked candidates reproduced"

rule "8. positive and negative control, and the decider guard"
decisions=$(sed -nE 's/^LIFT decisions=([0-9]+).*/\1/p' "$run_log" | head -1)
outcomes=$(sed -nE 's/^LIFT decisions=[0-9]+ outcomes=([0-9]+).*/\1/p' "$run_log" | head -1)
snapshots=$(sed -nE 's/^LIFT decisions=[0-9]+ outcomes=[0-9]+ snapshots=([0-9]+).*/\1/p' "$run_log" | head -1)
[ -n "$decisions" ] && [ "$decisions" -ge "$requests" ] || verdict 0 "the decider guard found ${decisions:-no} decisions, want >= $requests (see $run_log)"
[ -n "$outcomes" ] && [ "$outcomes" -ge "$requests" ] || verdict 0 "the decider guard found ${outcomes:-no} outcomes, want >= $requests (see $run_log)"
[ -n "$snapshots" ] && [ "$snapshots" -ge 1 ] || verdict 0 "the decider guard found no FleetSnapshot (see $run_log)"
grep -qE '^LIFT controls=positive:stub_served=[1-9][0-9]* negative:shed_recorded=true' "$run_log" \
  || verdict 0 "the positive or negative control did not hold (see $run_log)"
say "step 8: $decisions decision(s), $outcomes outcome(s), $snapshots snapshot(s); positive control served, negative control recorded the shed"

verdict 1 "the serving package resolved, built, passed its own tests and RAN outside the monorepo ($served/$requests served across $endpoints stub endpoint(s), $shed_count shed(s), $replayed/$replayable decisions replayed), with no inference backend in its closure"
