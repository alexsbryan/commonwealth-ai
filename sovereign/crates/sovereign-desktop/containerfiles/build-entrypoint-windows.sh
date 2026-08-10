#!/usr/bin/env bash
# build-entrypoint-windows.sh — runs inside Containerfile.windows-build.
#
# Cross-builds the Windows desktop bundle (NSIS -setup.exe + updater .sig)
# for x86_64-pc-windows-msvc via cargo-xwin, at native arm64 speed.
#
# Reads from env (all optional):
#   TAURI_SIGNING_PRIVATE_KEY            - base64 updater private key
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD   - password from `tauri signer generate`
#   SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH - "1" to skip PDFium/PaddleOCR fetch

set -euo pipefail

log() { printf '\n[build-desktop-windows] %s\n' "$*"; }

cd /work

TARGET="x86_64-pc-windows-msvc"
log "Windows container build starting on $(uname -m) (cross → $TARGET)"

# ─── Ensure the (pinned) toolchain has the Windows cross target ──────
# rust-toolchain.toml pins the channel (e.g. 1.95.0). The image bakes the
# $TARGET std into the image's DEFAULT `stable` toolchain, NOT the pin —
# so the moment the pin differs from the image's stable, cargo (which
# honours /work/rust-toolchain.toml) runs the pinned rustc, which lacks
# $TARGET, and `cargo tauri build --target $TARGET` dies with
# "target x86_64-pc-windows-msvc is not installed". Adding it to the
# ACTIVE (pinned) toolchain here decouples the build from BOTH the pin
# version and whatever the image happened to bake. Idempotent + offline
# after the first fetch. (The Linux leg needs no equivalent: it runs as
# linux/amd64, so its host target already IS the build target.)
log "Ensuring rustup target $TARGET is installed for the active (pinned) toolchain..."
rustup target add "$TARGET"

# ─── Stage external binaries (PDFium dll + PaddleOCR models) ─────────
# No tesseract sidecar: PaddleOCR replaced it (see RELEASING.md "Why
# PaddleOCR replaced tesseract"); the models + pdfium.dll ship as
# bundle.resources.
if [[ "${SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH:-0}" != "1" ]]; then
    log "Fetching PDFium + PaddleOCR models for $TARGET..."
    bash scripts/fetch-desktop-binaries.sh "$TARGET"
else
    log "Skipping binaries fetch (SOVEREIGN_DESKTOP_SKIP_BINARIES_FETCH=1)"
fi

# ─── Frontend deps ────────────────────────────────────────────────────
# node_modules is a container-private mount (see the driver) so this
# npm ci can't stomp the host's darwin natives.
log "Installing npm deps..."
(cd sovereign/crates/sovereign-desktop && npm ci --no-audit --no-fund)

# ─── Signing key sanity check ────────────────────────────────────────
if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
    log "WARNING: TAURI_SIGNING_PRIVATE_KEY not set — installer will be UNSIGNED (no .sig; updater will skip it)."
fi

# ─── Tauri cross build ────────────────────────────────────────────────
# --runner cargo-xwin: Tauri invokes `cargo-xwin build ...`, which wires
# the LLVM toolchain + the downloaded MSVC CRT/SDK per crate.
#
# Stale-cache hygiene: cargo-xwin creates clang-cl/lld-link symlinks
# inside its cache dir pointing at the image's real LLVM binaries. The
# cache is a persistent host mount, so after an image rebuild those
# symlinks can point at paths that no longer exist — and cargo-xwin
# hard-errors ("failed to symlink … File exists") instead of fixing
# them. They're per-run artifacts; always clear so xwin recreates them.
rm -f /work/.xwin-container/{clang,clang-cl,lld-link,llvm-rc,llvm-lib}

# llama-cpp-sys-4's Windows MAX_PATH workaround keys off the TARGET
# ("if target.contains(windows)") and redirects its cmake build tree to
# %LOCALAPPDATA%\llcb\<hash>, falling back to the literal "C:\Temp" when
# the env vars are absent — which is what happens cross-compiling from
# Linux, where cmake then chokes on the Windows-style path. Point the
# first var in its fallback chain at /tmp so the short-dir becomes a
# valid host path; MAX_PATH is a non-issue on a Linux host.
export LOCALAPPDATA=/tmp

# C++ flags: two cargo-xwin 0.23 clang-cl-mode defects repaired via
# cargo's [env] force=true (cc-rs reads one CXXFLAGS env var and
# cargo-xwin overwrites the process env when spawning cargo, so a plain
# export is clobbered; [env] force is applied inside cargo and wins;
# the value must be a FULL flag set — env replaces, not appends):
#
#   1. No /EHsc → any C++ touching the STL dies with "cannot use
#      'throw' with exceptions disabled".
#   2. Its stock CXXFLAGS aim C++ compiles at the windows-msvc-sysroot
#      project's bleeding-edge MSVC STL headers while the LINK resolves
#      msvcprt.lib from the xwin-downloaded CRT — a version skew whose
#      symptom is "undefined symbol: __std_max_element_4i" (and other
#      __std_* vectorized-algorithm exports) at lld-link time. Compile
#      against xwin's OWN CRT STL headers instead (same package as the
#      link libs; needs clang ≥19 per its STL1000 assert — the image
#      ships the apt.llvm.org snapshot). This is cargo-xwin's C-side
#      CL_FLAGS set + /EHsc, so C and C++ see one consistent CRT.
XWIN=/work/.xwin-container/xwin
CXX_WIN_FLAGS="--target=x86_64-pc-windows-msvc -Wno-unused-command-line-argument -fuse-ld=lld-link /imsvc $XWIN/crt/include /imsvc $XWIN/sdk/include/ucrt /imsvc $XWIN/sdk/include/um /imsvc $XWIN/sdk/include/shared /imsvc $XWIN/sdk/include/winrt /EHsc"
# CMake wrapper toolchain, fixing two defects in xwin's generated one
# (probe-validated: a Reg*-calling exe configures, links, and is a valid
# PE32+ binary under this wrapper):
#
#   1. Compiler identification fails ("could not open 'libcmt.lib'"):
#      xwin puts its -libpath flags in a custom LINK_FLAGS list that the
#      ID try-link never sees. CMAKE_EXE_LINKER_FLAGS_INIT is the
#      documented toolchain channel and reaches the ID stage — with it,
#      clang-cl identifies as "Clang … MSVC-like".
#   2. xwin DELIBERATELY empties CMAKE_{C,CXX}_STANDARD_LIBRARIES with
#      FORCE ("let projects explicitly control which libraries they
#      require") — but ggml relies on MSVC's implicit defaults, so its
#      Windows CPU detection dies at llama's cmake link with "undefined
#      symbol: RegOpenKeyExA" (advapi32). Restore the MSVC set; must
#      also be FORCE to overwrite xwin's forced empty cache entry.
#
# cmake-rs reads CMAKE_TOOLCHAIN_FILE_<target> from env (forced below,
# same [env] channel as CXXFLAGS).
XWIN_TOOLCHAIN=/work/.xwin-container/cmake/clang-cl/x86_64-pc-windows-msvc-toolchain.cmake
WRAPPER_TOOLCHAIN=/work/.xwin-container/cmake/sovereign-windows-toolchain.cmake
mkdir -p "$(dirname "$WRAPPER_TOOLCHAIN")"
cat > "$WRAPPER_TOOLCHAIN" <<EOF
# Generated by build-entrypoint-windows.sh — do not edit.
include("$XWIN_TOOLCHAIN")
set(CMAKE_EXE_LINKER_FLAGS_INIT "-libpath:$XWIN/crt/lib/x86_64 -libpath:$XWIN/sdk/lib/um/x86_64 -libpath:$XWIN/sdk/lib/ucrt/x86_64")
set(CMAKE_SHARED_LINKER_FLAGS_INIT "\${CMAKE_EXE_LINKER_FLAGS_INIT}")
set(CMAKE_MODULE_LINKER_FLAGS_INIT "\${CMAKE_EXE_LINKER_FLAGS_INIT}")
set(MSVC_STD_LIBS "kernel32.lib user32.lib gdi32.lib winspool.lib shell32.lib ole32.lib oleaut32.lib uuid.lib comdlg32.lib advapi32.lib")
set(CMAKE_C_STANDARD_LIBRARIES "\${MSVC_STD_LIBS}" CACHE STRING "" FORCE)
set(CMAKE_CXX_STANDARD_LIBRARIES "\${MSVC_STD_LIBS}" CACHE STRING "" FORCE)
EOF

mkdir -p "$CARGO_HOME"
cat > "$CARGO_HOME/config.toml" <<EOF
[env]
CXXFLAGS_x86_64_pc_windows_msvc = { value = "$CXX_WIN_FLAGS", force = true }
CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_msvc = { value = "$WRAPPER_TOOLCHAIN", force = true }
EOF
# ─── Canonical-case .lib aliases (LINUX HOSTS ONLY NEED THIS) ─────────
# xwin splats each import library under exactly TWO spellings: the real
# lowercase file plus an all-caps symlink (pathcch.lib + PATHCCH.lib). But a
# crate that emits `cargo:rustc-link-lib=DirectML` makes lld-link look for the
# CANONICAL mixed-case name, which xwin never creates. On macOS nobody notices:
# APFS is case-insensitive, so PathCch.lib resolves to pathcch.lib. On a
# case-sensitive Linux filesystem it does not, and the leg dies at link:
#
#     lld-link: error: could not open 'DirectML.lib': No such file or directory
#     lld-link: error: could not open 'PathCch.lib': No such file or directory
#
# (Observed 2026-08-10, the first time this leg ran on Linux. RELEASING.md's
# capability table claimed the Windows leg worked on both hosts; it had only
# ever been run on the Mac, where the filesystem was hiding this.)
#
# NOT a two-file patch. 70 of the 453 import libraries lack their canonical
# spelling, and which ones bite is decided by the dependency graph — the next
# crate to link DbgHelp or DWrite would reopen it. So the aliases are DERIVED,
# not listed: the SDK headers preserve canonical case (PathCch.h, DirectML.h),
# which makes the correct spelling recoverable for every lib that has one.
alias_canonical_libs() {
    local sdk="$1" made=0
    python3 - "$sdk" <<'PYEOF' || return 1
import os, sys, glob
sdk = sys.argv[1]
stems = {}
for d in ("sdk/include/um", "sdk/include/shared", "sdk/include/ucrt", "crt/include"):
    for p in glob.glob(os.path.join(sdk, d, "*.h")):
        b = os.path.basename(p)[:-2]
        stems.setdefault(b.lower(), set()).add(b)

made = total = 0
for libdir in glob.glob(os.path.join(sdk, "sdk/lib/um/*")) + glob.glob(os.path.join(sdk, "crt/lib/*")):
    if not os.path.isdir(libdir):
        continue
    present = set(os.listdir(libdir))
    for f in sorted(present):
        # Only the real lowercase entries; the all-caps ones are xwin's symlinks.
        if not f.endswith(".lib") or f != f.lower():
            continue
        for canon in stems.get(f[:-4], ()):
            alias = f"{canon}.lib"
            if canon == f[:-4] or alias in present:
                continue
            total += 1
            try:
                os.symlink(f, os.path.join(libdir, alias))
                present.add(alias)
                made += 1
            except FileExistsError:
                pass
print(f"canonical-case lib aliases: created {made}, already present {total - made}")
PYEOF
}

if [[ -d "$XWIN/sdk/lib/um" ]]; then
    log "Aliasing canonical-case import libraries (case-sensitive host fix)..."
    alias_canonical_libs "$XWIN"
else
    # Absence reported, never defaulted (ARCH §18.3). cargo-xwin splats the SDK
    # as a side effect of building and exposes no splat-only command, so on a
    # COLD cache there is nothing to alias yet. Say so, and name the remedy,
    # rather than skipping silently and letting the link error look mysterious.
    log "WARNING: $XWIN/sdk/lib/um does not exist yet — the MSVC SDK has not been"
    log "         splatted, so canonical-case lib aliases cannot be created on this"
    log "         run. This run populates the cache; if it dies at link with"
    log "         \"could not open 'SomeLib.lib'\", re-run and the aliases will be made."
fi

# --bundles nsis: the one Windows bundle type buildable off-Windows
# (makensis is cross-platform; WiX/.msi is not). Tauri downloads its
# NSIS plugins into the mounted tauri cache on first run.
log "Running cargo tauri build (cargo-xwin, NSIS)..."
(cd sovereign/crates/sovereign-desktop && cargo tauri build \
    --runner cargo-xwin \
    --target "$TARGET" \
    --bundles nsis \
    --config src-tauri/tauri.release.conf.json)

# ─── Report what landed ──────────────────────────────────────────────
BUNDLE_DIR="${CARGO_TARGET_DIR:-/work/target-container-windows}/${TARGET}/release/bundle"
log "Build complete. Bundles:"
shopt -s nullglob
for f in "$BUNDLE_DIR/nsis"/*.exe "$BUNDLE_DIR/nsis"/*.sig; do
    printf '  %s  %s\n' "$(stat -c '%s' "$f" | numfmt --to=iec --suffix=B --padding=8)" "$f"
done

log "Done."
