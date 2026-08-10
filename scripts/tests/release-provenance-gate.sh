#!/usr/bin/env bash
# Negative controls for the provenance gate in scripts/release-cli-local.sh.
#
# WHY THIS EXISTS. dist/ is not cleaned between releases and the upload list is
# a GLOB, so whatever an earlier release left behind is picked up and shipped
# under today's tag. Nothing downstream catches it: the filename is built from
# $VERSION, the .sha256 is regenerated from the stale bytes so it verifies, and
# the SHA256SUMS step deliberately preserves assets it did not rebuild.
#
# This is not hypothetical on either side of the house:
#   • desktop-v0.3.5 (2026-07-27) shipped a byte-identical 0.3.3 payload under a
#     0.3.5 name, correctly signed. Users who updated landed back on 0.3.3.
#   • cli-v0.5.0 (2026-08-08) was one successful build leg away from shipping
#     Jul-29 binaries for 2 of 3 targets; only an unrelated crash before the
#     upload step prevented it.
#
# The gate must distinguish FOUR verdicts, not two (ARCH §18.1): shippable,
# stale-version, stale-commit, unverifiable. A gate that only ever refuses is
# as useless as one that only ever accepts, so the first case here asserts a
# clean build IS accepted and uploaded.
#
# Runs the REAL script with `gh` stubbed, in a mktemp repo. Nothing is uploaded,
# nothing outside the temp dir is touched (ARCH §12.4).
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
REAL_SCRIPT="$ROOT/scripts/release-cli-local.sh"
[[ -f "$REAL_SCRIPT" ]] || { echo "cannot find $REAL_SCRIPT"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/root/scripts/lib" "$T/root/dist" "$T/bin"
cp "$REAL_SCRIPT" "$T/root/scripts/release-cli-local.sh"
# The driver sources its host decider; copy it too, or the temp repo
# tests a script that cannot start.
cp "$ROOT/scripts/lib/release-host.sh" "$T/root/scripts/lib/release-host.sh"
printf '[workspace.package]\nversion = "0.5.0"\n' > "$T/root/Cargo.toml"

# Stub gh: succeed for auth/view; mark uploads loudly so a gate MISS is visible
# as an upload that should never have happened.
cat > "$T/bin/gh" <<'EOF'
#!/usr/bin/env bash
# A minimal model of the three release-API calls the driver makes. The
# SHA256SUMS step reads the sidecars BACK OFF the release, so a stub that
# answers nothing there makes the driver look broken when it is not.
case "$*" in
  *"release upload"*)
      echo "!!! UPLOAD HAPPENED: $*" ;;
  *"release view"*"--json assets"*)
      # One asset name per line, as the driver's --template renders it.
      # GH_STUB_NO_ASSETS models a release that carries none.
      [ -n "${GH_STUB_NO_ASSETS:-}" ] && exit 0
      for f in dist/*.tar.gz.sha256; do [ -e "$f" ] && basename "$f"; done ;;
  *"release download"*)
      pattern=""; dir="."
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --pattern) pattern="$2"; shift ;;
          --dir)     dir="$2"; shift ;;
        esac
        shift
      done
      [ -n "$pattern" ] && cp "dist/$pattern" "$dir/$pattern" 2>/dev/null ;;
  *) exit 0 ;;
esac
exit 0
EOF
chmod +x "$T/bin/gh"
export PATH="$T/bin:$PATH"

cd "$T/root" || exit 2
git init -q .
git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
HEAD_SHA="$(git rev-parse HEAD)"

# `shasum` is macOS; `sha256sum` is GNU. Same output format, so the sidecar
# is identical either way — only the command name differs.
if command -v shasum >/dev/null 2>&1; then sha256_of() { shasum -a 256 "$@"; }
else sha256_of() { sha256sum "$@"; }; fi

rc=0

mk_tarball() {  # mk_tarball <triple> <version|NONE> <commit>
    local triple="$1" ver="$2" sha="$3"
    mkdir -p "dist/sovereign-$triple"
    echo payload > "dist/sovereign-$triple/sovereign-cli"
    tar -czf "dist/sovereign-$triple.tar.gz" -C dist "sovereign-$triple"
    ( cd dist && sha256_of "sovereign-$triple.tar.gz" > "sovereign-$triple.tar.gz.sha256" )
    [[ "$ver" == "NONE" ]] && return 0
    printf 'version=%s\ncommit=%s\ntriple=%s\n' "$ver" "$sha" "$triple" \
        > "dist/sovereign-$triple.tar.gz.buildinfo"
}

run_case() {  # run_case <name> <expect-pass|expect-die> <needle>
    local name="$1" expect="$2" needle="$3" out uploaded=0
    out="$(bash scripts/release-cli-local.sh --upload-only 2>&1)"; local code=$?
    grep -q 'UPLOAD HAPPENED' <<<"$out" && uploaded=1

    if [[ "$expect" == "expect-die" ]]; then
        if (( code != 0 )) && (( uploaded == 0 )) && grep -qi -- "$needle" <<<"$out"; then
            echo "  ok    $name — refused, nothing uploaded"
        else
            echo "  FAIL  $name — rc=$code uploaded=$uploaded; wanted refusal mentioning '$needle'"
            sed 's/^/          /' <<<"$out" | tail -6
            rc=1
        fi
    else
        if (( code == 0 )) && (( uploaded == 1 )); then
            echo "  ok    $name — accepted and uploaded"
        else
            echo "  FAIL  $name — rc=$code uploaded=$uploaded; wanted acceptance"
            sed 's/^/          /' <<<"$out" | tail -6
            rc=1
        fi
    fi
    rm -f dist/sovereign-*.tar.gz dist/sovereign-*.sha256 dist/sovereign-*.buildinfo
}

echo "release-provenance-gate:"

mk_tarball aarch64-apple-darwin 0.5.0 "$HEAD_SHA"
run_case "a current build is shippable" expect-pass ""

mk_tarball aarch64-apple-darwin 0.4.0 "$HEAD_SHA"
run_case "stale VERSION (0.4.0 shipped as 0.5.0)" expect-die "STALE artifact"

mk_tarball aarch64-apple-darwin 0.5.0 "0000000000000000000000000000000000000000"
run_case "stale COMMIT (right version, older code)" expect-die "different code"

mk_tarball aarch64-apple-darwin NONE ""
run_case "no sidecar is unverifiable, not assumed good" expect-die "unverifiable"

# The exact cli-v0.5.0 shape: one fresh leg beside one left over from v0.4.x.
mk_tarball aarch64-apple-darwin 0.5.0 "$HEAD_SHA"
mk_tarball x86_64-apple-darwin  0.4.0 "$HEAD_SHA"
run_case "one fresh + one stale refuses the WHOLE upload" expect-die "STALE artifact"

# SHA256SUMS is rebuilt by concatenating every .sha256 sidecar found ON the
# release. `shopt -s nullglob` is in force by then, so a release carrying no
# sidecars expanded that glob to NOTHING and left a bare `cat` — which reads
# STDIN. Reverting the fix and re-running this case shows BOTH failure modes,
# neither of which is an error:
#   • attached to a terminal, the driver BLOCKS forever, after having already
#     uploaded the tarballs (this is how it was found);
#   • with stdin closed, `cat` hits EOF immediately and the driver exits 0
#     having published an EMPTY SHA256SUMS over the real one.
# So the assertion is a refusal, and `timeout` distinguishes the first mode
# from a slow pass: rc=124 is a hang, and a hang is a failure.
mk_tarball aarch64-apple-darwin 0.5.0 "$HEAD_SHA"
out="$(GH_STUB_NO_ASSETS=1 timeout 20 bash scripts/release-cli-local.sh --upload-only </dev/null 2>&1)"
code=$?
if (( code == 124 )); then
    echo "  FAIL  an empty sidecar listing must fail, not hang — timed out after 20s"
    rc=1
elif (( code != 0 )) && grep -qi 'no .sha256 sidecars' <<<"$out"; then
    echo "  ok    an empty sidecar listing fails loudly instead of blocking on stdin"
else
    echo "  FAIL  empty sidecar listing — rc=$code; wanted a refusal naming the missing sidecars"
    sed 's/^/          /' <<<"$out" | tail -6
    rc=1
fi
rm -f dist/sovereign-*.tar.gz dist/sovereign-*.sha256 dist/sovereign-*.buildinfo

exit "$rc"
