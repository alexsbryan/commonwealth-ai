#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the llama-server the daemon ships, from PIN.toml, and install it beside
# the daemon with the manifest the daemon checks at startup. Runs inside the
# sovereign-vulkan toolbox.
#
#   build.sh [--backend B] [--install-dir DIR] [--work-dir W]
#                                      the shipped build; DIR defaults to
#                                      target/debug, where sovereign-cli-daemon is.
#                                      W (source and build trees) defaults to
#                                      target/llama-server; a container sees the
#                                      repo at another path, so it needs its own,
#                                      or CMake refuses the host's cache
#                                      B is vulkan (the Linux default), metal
#                                      (the macOS arm64 default), cpu (the
#                                      macOS x86_64 default), rocm or cuda
#   build.sh --vanilla [--llguidance]  an unpatched server for the bench's A/B
#                                      arms, in target/llama-server-vanilla/
#                                      build/ (build-llg/ with llguidance); no
#                                      install, no manifest
#
# The shipped build applies every patch PIN.toml lists, after checking each
# file's sha256 against the pin, turns on the RPC backend, and links llama.cpp
# statically, so each binary PIN.toml names (llama-server, ggml-rpc-server) is
# whole on its own. It copies them beside the daemon and writes
# llama-server.manifest.toml beside them: PIN.toml verbatim, then a [build]
# table naming the backend. The daemon compares the manifest's pin with the one it was compiled
# with (llama_router::build_pin), because the binary cannot say: its
# `--version` names the enclosing repo's HEAD, since cmake reads the git repo
# that target/ sits in.
#
# The tree comes from codeload at the pinned SHA. The vendored tree cannot
# build the server (it keeps only tools/server's CMakeLists.txt and
# server.cpp).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
pin=$here/PIN.toml
vanilla= llg_vanilla=OFF install_dir=$repo/target/debug backend= work_dir=$repo/target/llama-server
while [ $# -gt 0 ]; do
  case $1 in
    --vanilla) vanilla=1 ;;
    --llguidance) llg_vanilla=ON ;;
    --install-dir) install_dir=$2; shift ;;
    --backend) backend=$2; shift ;;
    --work-dir) work_dir=$2; shift ;;
    *) echo "build.sh: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

# llama.cpp builds llguidance with a nested cargo and reads llguidance.h from
# that cargo's default target dir; an inherited CARGO_TARGET_DIR (the release
# container sets one) sends the header elsewhere and the build fails.
unset CARGO_TARGET_DIR

# Run a build step into its log; on failure, show the log's tail.
logged() {
  log=$1; shift
  "$@" > "$log" 2>&1 || { tail -30 "$log" >&2; echo "build.sh: failed, full log $log" >&2; exit 1; }
}

# nproc and sha256sum are GNU; macOS has getconf and shasum.
jobs=$(getconf _NPROCESSORS_ONLN)
sha256() {
  if command -v sha256sum > /dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi | cut -d' ' -f1
}

# `key = "value"` or `key = value`, first match.
field() { sed -n "s/^$1 = \"\{0,1\}\([^\"]*\)\"\{0,1\}\$/\1/p" "$pin" | head -1; }
sha=$(field llama_cpp_commit)
case $sha in
  [0-9a-f]*) [ ${#sha} -eq 40 ] || { echo "build.sh: llama_cpp_commit in $pin is not a 40-char sha" >&2; exit 2; } ;;
  *) echo "build.sh: no llama_cpp_commit in $pin" >&2; exit 2 ;;
esac
# The conformance battery compares the embedded engine with this server, so
# while the embedded engine is vendored the two are built from one commit.
vendored=$repo/vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT
if [ -f "$vendored" ] && [ "$(head -1 "$vendored")" != "$sha" ]; then
  echo "build.sh: $pin pins $sha, the embedded engine vendors $(head -1 "$vendored") ($vendored)" >&2
  exit 2
fi

# The pristine tarball, fetched once per SHA into $1.
fetch() {
  tarball=$1/llama.cpp-$sha.tar.gz
  mkdir -p "$1"
  if [ ! -f "$tarball" ]; then
    curl -sSfL -o "$tarball.part" "https://codeload.github.com/ggml-org/llama.cpp/tar.gz/$sha"
    mv "$tarball.part" "$tarball"
  fi
  tarball_sha256=$(sha256 "$tarball")
}

if [ -n "$vanilla" ]; then
  dir=$repo/target/llama-server-vanilla
  fetch "$dir"
  src=$dir/llama.cpp-$sha
  [ -d "$src" ] || tar -xzf "$tarball" -C "$dir"
  printf 'commit %s\ntarball_sha256 %s\n' "$sha" "$tarball_sha256" > "$dir/source.txt"
  build=build
  [ $llg_vanilla = ON ] && build=build-llg
  logged "$dir/$build-configure.log" cmake -S "$src" -B "$dir/$build" -DGGML_VULKAN=ON -DLLAMA_CURL=OFF \
    -DLLAMA_LLGUIDANCE=$llg_vanilla -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DCMAKE_BUILD_TYPE=Release
  logged "$dir/$build-build.log" cmake --build "$dir/$build" --target llama-server -j "$jobs"
  echo "build.sh: $dir/$build/bin/llama-server from llama.cpp $sha, unpatched (llguidance $llg_vanilla)"
  exit 0
fi

# Every listed patch must match its digest, and every patch on disk must be
# listed: a patch edited or added without the pin would ship unrecorded.
listed=$(sed -n 's/^file = "\(.*\)"$/\1/p' "$pin")
digests=$(sed -n 's/^sha256 = "\(.*\)"$/\1/p' "$pin")
[ "$(printf '%s\n' "$listed" | grep -c .)" -eq "$(printf '%s\n' "$digests" | grep -c .)" ] ||
  { echo "build.sh: $pin lists a patch without a sha256" >&2; exit 2; }
for f in "$here"/patches/*.patch; do
  [ -e "$f" ] || continue
  printf '%s\n' "$listed" | grep -qxF "$(basename "$f")" ||
    { echo "build.sh: $f is not listed in $pin" >&2; exit 2; }
done
i=0
for file in $listed; do
  i=$((i + 1))
  want=$(printf '%s\n' "$digests" | sed -n "${i}p")
  got=$(sha256 "$here/patches/$file")
  [ "$got" = "$want" ] || { echo "build.sh: patches/$file is sha256 $got, $pin says $want" >&2; exit 3; }
done

case $(field llguidance) in
  true) llg=ON ;;
  false) llg=OFF ;;
  *) echo "build.sh: llguidance in $pin is not true or false" >&2; exit 2 ;;
esac
binaries=$(sed -n 's/^binaries = \[\(.*\)\]$/\1/p' "$pin" | tr ',"' '  ')
[ -n "$binaries" ] || { echo "build.sh: no binaries list in $pin" >&2; exit 2; }

# The backend's flags. The macOS modes match the embedded engine's build
# (vendor/llama-cpp-sys-4/build.rs): no OpenMP, 10.15, and GGML_NATIVE off with
# its portable CPU baseline, since a release runs on Macs other than the
# builder. The pod modes are portable too; GPU_TARGETS and
# CMAKE_CUDA_ARCHITECTURES pass through from the Containerfiles' env.
if [ -z "$backend" ]; then
  case $(uname -s)/$(uname -m) in
    Darwin/arm64) backend=metal ;;
    Darwin/x86_64) backend=cpu ;;
    *) backend=vulkan ;;
  esac
fi
mac="-DGGML_OPENMP=OFF -DCMAKE_OSX_DEPLOYMENT_TARGET=10.15 -DGGML_NATIVE=OFF"
case $backend in
  vulkan) flags="-DGGML_VULKAN=ON" ;;
  metal) flags="-DGGML_METAL=ON -DGGML_METAL_EMBED_LIBRARY=ON $mac -DGGML_CPU_ARM_ARCH=armv8-a" ;;
  cpu) flags="-DGGML_METAL=OFF $mac -DGGML_SSE42=ON -DGGML_AVX=OFF -DGGML_AVX2=OFF -DGGML_AVX_VNNI=OFF
    -DGGML_FMA=OFF -DGGML_F16C=OFF -DGGML_BMI2=OFF -DGGML_AVX512=OFF"
    # The x86_64 leg cross-built on an arm64 Mac (release-cli-local.sh), which
    # needs Rosetta for the build's own x86_64 tools. The nested llguidance
    # cargo builds for its host into target/release, where CMake reads it, so
    # it runs as the x86_64 toolchain. Homebrew's OpenSSL and the SDK's librdma
    # (ggml-rpc's RDMA-over-Thunderbolt) are arm64 only, so HTTPS and RDMA go:
    # the daemon hands the server local model paths and RPC runs over TCP.
    if [ "$(uname -s)/$(uname -m)" = Darwin/arm64 ]; then
      export RUSTUP_TOOLCHAIN=stable-x86_64-apple-darwin
      flags="$flags -DCMAKE_OSX_ARCHITECTURES=x86_64 -DLLAMA_OPENSSL=OFF -DGGML_RPC_RDMA=OFF"
      echo "build.sh: cross-building x86_64 on arm64: llguidance with $RUSTUP_TOOLCHAIN, OpenSSL and RPC RDMA off" >&2
    fi ;;
  # No PIE, as serve/container/Containerfile links the daemon: ggml-hip's
  # static archive carries R_X86_64_32 relocations a PIE link rejects.
  rocm) flags="-DGGML_HIP=ON -DGGML_NATIVE=OFF -DCMAKE_EXE_LINKER_FLAGS=-no-pie${GPU_TARGETS:+ -DGPU_TARGETS=$GPU_TARGETS}" ;;
  # NCCL off as Containerfile.cuda sets it for the embedded engine: with it on,
  # ggml_cuda_init's ncclCommInitAll fails on pod hosts whose driver and NCCL
  # disagree, and a single-GPU pod has no use for it.
  cuda) flags="-DGGML_CUDA=ON -DGGML_CUDA_NCCL=OFF -DGGML_NATIVE=OFF${CMAKE_CUDA_ARCHITECTURES:+ -DCMAKE_CUDA_ARCHITECTURES=$CMAKE_CUDA_ARCHITECTURES}" ;;
  *) echo "build.sh: unknown backend $backend (vulkan, metal, cpu, rocm, cuda)" >&2; exit 2 ;;
esac

# The source tree is keyed by what the pin builds (commit, llguidance, patch
# digests), the build dir by that and the backend, so a changed pin builds
# fresh and an unchanged one, comments included, rebuilds incrementally.
dir=$work_dir
key=$(printf '%s\n' "$sha" "$llg" $digests | sha256 | cut -c1-12)
fetch "$dir"
src=$dir/src-$key
if [ ! -d "$src" ]; then
  rm -rf "$src.part"
  mkdir -p "$src.part"
  tar -xzf "$tarball" -C "$src.part" --strip-components=1
  for file in $listed; do
    patch -d "$src.part" -p1 --quiet < "$here/patches/$file"
  done
  mv "$src.part" "$src"
fi
build=$dir/build-$key-$backend
# $flags and $binaries are word lists, split on purpose.
# shellcheck disable=SC2086
logged "$build-configure.log" cmake -S "$src" -B "$build" $flags -DGGML_RPC=ON -DLLAMA_CURL=OFF \
  -DLLAMA_LLGUIDANCE=$llg -DBUILD_SHARED_LIBS=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
  -DCMAKE_BUILD_TYPE=Release
# shellcheck disable=SC2086
logged "$build-build.log" cmake --build "$build" --target $binaries -j "$jobs"

# The manifest goes last: it is what the daemon checks, so it never names a
# binary that is not yet in place.
mkdir -p "$install_dir"
for bin in $binaries; do
  cp "$build/bin/$bin" "$install_dir/.$bin.part"
  mv "$install_dir/.$bin.part" "$install_dir/$bin"
done
{
  cat "$pin"
  printf '\n[build]\nbackend = "%s"\ntarball_sha256 = "%s"\nsource = "%s"\n' "$backend" "$tarball_sha256" "$src"
} > "$install_dir/.llama-server.manifest.part"
mv "$install_dir/.llama-server.manifest.part" "$install_dir/llama-server.manifest.toml"
echo "build.sh:" $binaries "in $install_dir from llama.cpp $sha, $(printf '%s\n' "$listed" | grep -c .) patch(es), llguidance $llg, backend $backend"
