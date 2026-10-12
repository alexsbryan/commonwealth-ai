#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the llama-server the daemon ships, from PIN.toml, and install it beside
# the daemon with the manifest the daemon checks at startup. Runs inside the
# sovereign-vulkan toolbox.
#
#   build.sh [--install-dir DIR]       the shipped build; DIR defaults to
#                                      target/debug, where sovereign-cli-daemon is
#   build.sh --vanilla [--llguidance]  an unpatched server for the bench's A/B
#                                      arms, in target/llama-server-vanilla/
#                                      build/ (build-llg/ with llguidance); no
#                                      install, no manifest
#
# The shipped build applies every patch PIN.toml lists, after checking each
# file's sha256 against the pin, and links llama.cpp statically, so the one
# file copied beside the daemon is the whole server. It then writes
# llama-server.manifest.toml beside it: PIN.toml verbatim, then a [build]
# table. The daemon compares the manifest's pin with the one it was compiled
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
vanilla= llg_vanilla=OFF install_dir=$repo/target/debug
while [ $# -gt 0 ]; do
  case $1 in
    --vanilla) vanilla=1 ;;
    --llguidance) llg_vanilla=ON ;;
    --install-dir) install_dir=$2; shift ;;
    *) echo "build.sh: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done

# Run a build step into its log; on failure, show the log's tail.
logged() {
  log=$1; shift
  "$@" > "$log" 2>&1 || { tail -30 "$log" >&2; echo "build.sh: failed, full log $log" >&2; exit 1; }
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
  tarball_sha256=$(sha256sum "$tarball" | cut -d' ' -f1)
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
  logged "$dir/$build-build.log" cmake --build "$dir/$build" --target llama-server -j "$(nproc)"
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
  got=$(sha256sum "$here/patches/$file" | cut -d' ' -f1)
  [ "$got" = "$want" ] || { echo "build.sh: patches/$file is sha256 $got, $pin says $want" >&2; exit 3; }
done

case $(field llguidance) in
  true) llg=ON ;;
  false) llg=OFF ;;
  *) echo "build.sh: llguidance in $pin is not true or false" >&2; exit 2 ;;
esac

# The source tree and build dir are keyed by what the pin builds (commit,
# llguidance, patch digests), so a changed pin builds fresh and an unchanged
# one, comments included, rebuilds incrementally.
dir=$repo/target/llama-server
key=$(printf '%s\n' "$sha" "$llg" $digests | sha256sum | cut -c1-12)
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
build=$dir/build-$key
logged "$build-configure.log" cmake -S "$src" -B "$build" -DGGML_VULKAN=ON -DLLAMA_CURL=OFF \
  -DLLAMA_LLGUIDANCE=$llg -DBUILD_SHARED_LIBS=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
  -DCMAKE_BUILD_TYPE=Release
logged "$build-build.log" cmake --build "$build" --target llama-server -j "$(nproc)"

mkdir -p "$install_dir"
cp "$build/bin/llama-server" "$install_dir/.llama-server.part"
mv "$install_dir/.llama-server.part" "$install_dir/llama-server"
{
  cat "$pin"
  printf '\n[build]\ntarball_sha256 = "%s"\nsource = "%s"\n' "$tarball_sha256" "$src"
} > "$install_dir/.llama-server.manifest.part"
mv "$install_dir/.llama-server.manifest.part" "$install_dir/llama-server.manifest.toml"
echo "build.sh: $install_dir/llama-server from llama.cpp $sha, $(printf '%s\n' "$listed" | grep -c .) patch(es), llguidance $llg"
