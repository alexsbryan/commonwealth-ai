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
mkdir -p "$T/root/scripts" "$T/root/dist" "$T/bin"
cp "$REAL_SCRIPT" "$T/root/scripts/release-cli-local.sh"
printf '[workspace.package]\nversion = "0.5.0"\n' > "$T/root/Cargo.toml"

# Stub gh: succeed for auth/view; mark uploads loudly so a gate MISS is visible
# as an upload that should never have happened.
cat > "$T/bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *"release upload"*) echo "!!! UPLOAD HAPPENED: $*" ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$T/bin/gh"
export PATH="$T/bin:$PATH"

cd "$T/root" || exit 2
git init -q .
git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
HEAD_SHA="$(git rev-parse HEAD)"

rc=0

mk_tarball() {  # mk_tarball <triple> <version|NONE> <commit>
    local triple="$1" ver="$2" sha="$3"
    mkdir -p "dist/sovereign-$triple"
    echo payload > "dist/sovereign-$triple/sovereign-cli"
    tar -czf "dist/sovereign-$triple.tar.gz" -C dist "sovereign-$triple"
    ( cd dist && shasum -a 256 "sovereign-$triple.tar.gz" > "sovereign-$triple.tar.gz.sha256" )
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

exit "$rc"
