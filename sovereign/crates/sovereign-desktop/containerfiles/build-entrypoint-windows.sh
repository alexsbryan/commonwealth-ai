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
