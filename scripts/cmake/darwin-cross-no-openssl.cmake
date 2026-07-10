# Toolchain fragment for CROSS macOS builds (e.g. x86_64-apple-darwin on an
# arm64 host). Injected via the CMAKE_TOOLCHAIN_FILE *environment variable*
# by scripts/build-desktop-macos.sh — llama-cpp-sys-4's build.rs passes no
# -DCMAKE_TOOLCHAIN_FILE on apple targets, so cmake (>=3.21) picks this up.
#
# Why: llama.cpp's LLAMA_OPENSSL defaults ON and find_package(OpenSSL) finds
# the HOST-arch Homebrew dylibs (/opt/homebrew, arm64). Linking the mtmd-
# feature tool binaries (llama-tts etc.) against them fails with
# "_X509_* symbol(s) not found for architecture x86_64" (desktop-v0.1.19
# Intel cut, 2026-07-10). The library itself never needs TLS — turn it off.
set(LLAMA_OPENSSL OFF CACHE BOOL "no host-arch OpenSSL in cross builds" FORCE)
