#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build arm A's llama-server from the llama.cpp commit the daemon vendors,
# unpatched, with the Vulkan backend. Runs inside the sovereign-vulkan toolbox.
#
# The commit is read from vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT line 1, so
# arm A follows the daemon's pin: both sides run the same kernels and the A/B
# measures what the daemon adds, not a llama.cpp revision gap. The vendored
# tree itself cannot build the server (it keeps only tools/server's
# CMakeLists.txt and server.cpp), so the full tree comes from codeload at that
# SHA. Output: target/llama-server-vanilla/build/bin/llama-server, linked to
# its own libggml (check with ldd: a system libggml would mix revisions).
#
#   build-llama-server.sh [--llguidance]
#
# --llguidance builds a second server into build-llg/ with LLAMA_LLGUIDANCE=ON,
# which `%llguidance` grammars need: the plain build GGML_ABORTs on one and
# dies. The engine-swap test (bench/lanes/engine-swap) uses that build.
#
# The binary's `--version` names the enclosing repo's HEAD, not the llama.cpp
# commit: cmake reads the git repo that target/ sits in. The tarball's sha256
# in source.txt is the record of what was built.
set -eu
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
sha=$(head -1 "$repo/vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT")
dir=$repo/target/llama-server-vanilla
src=$dir/llama.cpp-$sha
mkdir -p "$dir"
if [ ! -d "$src" ]; then
  curl -sSfL -o "$dir/llama.cpp-$sha.tar.gz" "https://codeload.github.com/ggml-org/llama.cpp/tar.gz/$sha"
  tar -xzf "$dir/llama.cpp-$sha.tar.gz" -C "$dir"
fi
printf 'commit %s\ntarball_sha256 %s\n' "$sha" \
  "$(sha256sum "$dir/llama.cpp-$sha.tar.gz" | cut -d' ' -f1)" > "$dir/source.txt"
build=build llg=OFF
[ "${1:-}" = --llguidance ] && build=build-llg llg=ON
cmake -S "$src" -B "$dir/$build" -DGGML_VULKAN=ON -DLLAMA_CURL=OFF -DLLAMA_LLGUIDANCE=$llg \
  -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DCMAKE_BUILD_TYPE=Release > "$dir/$build-configure.log"
cmake --build "$dir/$build" --target llama-server -j "$(nproc)" > "$dir/$build-build.log"
echo "build-llama-server: $dir/$build/bin/llama-server from llama.cpp $sha (llguidance $llg)"
cat "$dir/source.txt"
