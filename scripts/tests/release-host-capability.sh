#!/usr/bin/env bash
# The release drivers must cut the legs THIS host can build, skip the ones it
# cannot BY NAME, and refuse a host they don't know.
#
# WHY THIS EXISTS. All three drivers used to open with
#
#     [[ "$(uname -sm)" == "Darwin arm64" ]] || die "assumes an arm64 Mac host"
#
# so an x86_64 Linux workstation could not cut the Linux CLI tarball, the
# AppImage/deb/rpm, or the Windows installer — three legs it builds NATIVELY,
# while the Mac emulates them under qemu (which is why the shader compile is
# pinned to one core there). The Apple legs genuinely cannot move: they need
# the macOS SDK. So the fix is a capability, not a looser gate, and the thing
# that has to be proven is that the capability discriminates:
#
#   • a non-Apple host announces the skip instead of skipping silently
#     (ARCH §18.3 — an auto-skip that reads identically to an operator's
#     --skip flag hides half a release from the person about to publish it);
#   • it does NOT touch the Apple toolchain (`xcrun` does not exist there,
#     and calling it unconditionally is precisely what killed the script
#     before it could reach the leg it CAN build);
#   • an Apple host does NOT get the auto-skip — otherwise the "fix" would
#     just skip the macOS build everywhere and every release would ship
#     without a .dmg;
#   • the qemu-only glslc cap follows the EMULATION, not the platform string,
#     which is linux/amd64 on both hosts;
#   • an unrecognised host is still refused.
#
# `uname` is stubbed to drive the host kind; no build, container, or upload
# runs. Cost: under a second.
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
LIB="$ROOT/scripts/lib/release-host.sh"
DRIVER="$ROOT/scripts/release-cli-local.sh"
for f in "$LIB" "$DRIVER"; do [[ -f "$f" ]] || { echo "cannot find $f"; exit 2; }; done

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/root/scripts/lib" "$T/root/dist" "$T/bin"
cp "$DRIVER" "$T/root/scripts/release-cli-local.sh"
cp "$LIB"    "$T/root/scripts/lib/release-host.sh"
printf '[workspace.package]\nversion = "0.5.0"\n' > "$T/root/Cargo.toml"

# Stubs. `xcrun` leaves a marker file rather than just succeeding, so "the
# Apple toolchain was consulted" is an observable fact and not an inference.
cat > "$T/bin/xcrun" <<EOF
#!/usr/bin/env bash
touch "$T/xcrun-was-called"
echo /fake/sdk
EOF
cat > "$T/bin/gh" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$T/bin/xcrun" "$T/bin/gh"

# `uname` is what the lib reads to decide capability. UNAME_SM selects it.
cat > "$T/bin/uname" <<'EOF'
#!/usr/bin/env bash
[[ "${1:-}" == "-sm" ]] && { echo "${UNAME_SM:-Linux x86_64}"; exit 0; }
exec /usr/bin/uname "$@"
EOF
chmod +x "$T/bin/uname"
export PATH="$T/bin:$PATH"

cd "$T/root" || exit 2
git init -q .
git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init

rc=0
pass() { echo "  ok    $1"; }
fail() { echo "  FAIL  $1"; rc=1; }

echo "release-host-capability:"

# ─── The lib's own verdicts, per host ─────────────────────────────────
probe() {  # probe <uname-sm> <var-or-expr> → value
    UNAME_SM="$2" bash -c '. scripts/lib/release-host.sh; eval "echo $1"' _ "$1" 2>&1
}

expect() {  # expect <name> <actual> <wanted>
    if [[ "$2" == "$3" ]]; then pass "$1"; else fail "$1 — got '$2', wanted '$3'"; fi
}

expect "mac-arm64 can build the Apple legs"      "$(probe '$RELEASE_CAN_APPLE'  'Darwin arm64')" 1
expect "linux-x86_64 cannot build the Apple legs" "$(probe '$RELEASE_CAN_APPLE' 'Linux x86_64')" 0
expect "mac-arm64 runs the linux leg emulated"    "$(probe '$RELEASE_LINUX_LEG_EMULATED' 'Darwin arm64')" 1
expect "linux-x86_64 runs the linux leg native"   "$(probe '$RELEASE_LINUX_LEG_EMULATED' 'Linux x86_64')" 0
expect "unknown host is unsupported"              "$(probe '$RELEASE_HOST_KIND' 'Linux aarch64')" unsupported

# The Windows image is host-arch (cargo-xwin cross-compiles from anywhere);
# hardcoding arm64 would have emulated the whole leg on Linux.
expect "windows container follows the host arch (mac)"   "$(probe '$RELEASE_HOST_CONTAINER_PLATFORM' 'Darwin arm64')" linux/arm64
expect "windows container follows the host arch (linux)" "$(probe '$RELEASE_HOST_CONTAINER_PLATFORM' 'Linux x86_64')" linux/amd64

# The glslc cap is a qemu workaround. Keyed to the platform string it would
# also fire natively and hand a 32-core box one core for the longest leg.
expect "emulated linux leg is capped to one cpu" \
    "$(probe '$(release_linux_build_cpus)' 'Darwin arm64')" 1
NPROC="$(nproc 2>/dev/null || echo 8)"
expect "native linux leg is not capped" \
    "$(probe '$(release_linux_build_cpus)' 'Linux x86_64')" "$NPROC"
expect "an explicit override beats both defaults" \
    "$(UNAME_SM='Darwin arm64' SOVEREIGN_LINUX_BUILD_CPUS=4 bash -c '. scripts/lib/release-host.sh; release_linux_build_cpus')" 4

# The BSD/GNU shims must return a NUMBER on the host actually running this —
# `df -g`, `du -sg` and `stat -f %m` each fail differently on Linux, and two
# of the three fail by producing something that explodes inside (( … )).
for probe_fn in "release_free_gb ." "release_dir_gb ." "release_file_mtime Cargo.toml"; do
    # shellcheck disable=SC2086
    v="$(bash -c ". scripts/lib/release-host.sh; $probe_fn" 2>&1)"
    if [[ "$v" =~ ^[0-9]+$ ]] && (( v > 0 )); then
        pass "${probe_fn%% *} answers a number on the real host ($v)"
    else
        fail "${probe_fn%% *} — got '$v', wanted a positive integer"
    fi
done

# ─── The driver's behaviour, per host ─────────────────────────────────
# The rc has to travel back through a command substitution, so run_driver
# RETURNS it rather than setting a variable the subshell would keep.
run_driver() {  # run_driver <uname-sm> [args…] → stdout+stderr, exit = driver's
    rm -f "$T/xcrun-was-called"
    local u="$1"; shift
    UNAME_SM="$u" bash scripts/release-cli-local.sh "$@" 2>&1
}

out="$(run_driver 'Linux x86_64' --skip-linux --no-upload)"; DRIVER_RC=$?
if (( DRIVER_RC == 0 )) && grep -q 'HOST CANNOT BUILD APPLE LEGS' <<<"$out"; then
    pass "linux host announces the Apple skip and keeps going"
else
    fail "linux host — rc=$DRIVER_RC, wanted rc=0 and a named Apple skip"
    sed 's/^/          /' <<<"$out" | tail -4
fi
if [[ -f "$T/xcrun-was-called" ]]; then
    fail "linux host must not consult xcrun (it does not exist there)"
else
    pass "linux host never touches the Apple toolchain"
fi

out="$(run_driver 'Darwin arm64' --skip-macos-arm --skip-macos-intel --skip-linux --no-upload)"; DRIVER_RC=$?
if grep -q 'HOST CANNOT BUILD APPLE LEGS' <<<"$out"; then
    fail "mac host must NOT be told it cannot build the Apple legs"
else
    pass "mac host keeps its Apple capability"
fi
if [[ -f "$T/xcrun-was-called" ]]; then
    pass "mac host resolves the SDK through xcrun"
else
    fail "mac host — xcrun was never called, so SDKROOT would be unset"
fi

# An operator's own --skip is not a host limitation and must not be narrated
# as one, or the announcement stops meaning anything.
out="$(run_driver 'Linux x86_64' --skip-macos-arm --skip-macos-intel --skip-linux --no-upload)"; DRIVER_RC=$?
if grep -q 'HOST CANNOT BUILD APPLE LEGS' <<<"$out"; then
    fail "explicitly-skipped Apple legs should not re-announce the host limit"
else
    pass "explicit --skip stays the operator's, not the host's"
fi

out="$(run_driver 'Linux aarch64' --no-upload)"; DRIVER_RC=$?
if (( DRIVER_RC != 0 )) && grep -q 'unsupported release host' <<<"$out"; then
    pass "an unknown host is refused, not guessed at"
else
    fail "unknown host — rc=$DRIVER_RC, wanted a refusal naming the host"
fi

exit "$rc"
