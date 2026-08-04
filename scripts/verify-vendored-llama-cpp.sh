#!/usr/bin/env bash
#
# Verify that vendor/llama-cpp-sys-4/llama.cpp is byte-identical to the upstream
# llama.cpp commit named in vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT.
#
# WHY THIS EXISTS. The vendored tree carries no commit marker of its own —
# llama.cpp fills BUILD_COMMIT at build time from a `.git` that is not present
# in a vendored copy. Without a check, "this is upstream <sha>" is an assertion
# in a comment, and a stray edit to a 34 MB tree is invisible. This turns the
# claim into something that can fail.
#
# Exit codes:
#   0  every vendored file matches upstream
#   1  at least one file differs, is absent upstream, or is missing locally
#   2  could not run the comparison (no network, bad SHA, missing tooling)
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="${REPO_ROOT}/vendor/llama-cpp-sys-4"
LLAMA_DIR="${VENDOR_DIR}/llama.cpp"
COMMIT_FILE="${VENDOR_DIR}/LLAMA_CPP_COMMIT"

fail_setup() { printf 'verify-vendored-llama-cpp: %s\n' "$1" >&2; exit 2; }

[[ -d "${LLAMA_DIR}" ]]   || fail_setup "no vendored tree at ${LLAMA_DIR}"
[[ -f "${COMMIT_FILE}" ]] || fail_setup "no commit marker at ${COMMIT_FILE}"
command -v curl >/dev/null || fail_setup "curl not found"
command -v tar  >/dev/null || fail_setup "tar not found"

# Line 1 is the SHA; everything after is prose.
SHA="$(head -n 1 "${COMMIT_FILE}" | tr -d '[:space:]')"
[[ "${SHA}" =~ ^[0-9a-f]{40}$ ]] || fail_setup "line 1 of LLAMA_CPP_COMMIT is not a 40-char sha: '${SHA}'"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

printf 'comparing %s\n' "${LLAMA_DIR#"${REPO_ROOT}"/}"
printf '   against ggml-org/llama.cpp@%s\n\n' "${SHA}"

URL="https://codeload.github.com/ggml-org/llama.cpp/tar.gz/${SHA}"
curl -sSfL --max-time 600 "${URL}" -o "${WORK}/up.tgz" \
  || fail_setup "download failed: ${URL} (offline, or the sha is not on a fetchable ref)"

mkdir -p "${WORK}/upstream"
tar xzf "${WORK}/up.tgz" -C "${WORK}/upstream" --strip-components=1 \
  || fail_setup "could not extract the upstream tarball"

same=0 differ=0 absent=0
# The published crate ships a SUBSET of upstream (it strips tests/, models/,
# and other non-build paths), so we only ever walk what is vendored. A file
# present upstream but not here is expected and is not reported.
while IFS= read -r -d '' path; do
    rel="${path#"${LLAMA_DIR}"/}"
    up="${WORK}/upstream/${rel}"
    if [[ ! -f "${up}" ]]; then
        absent=$((absent + 1)); printf '  NOT-IN-UPSTREAM  %s\n' "${rel}"
    elif cmp -s "${path}" "${up}"; then
        same=$((same + 1))
    else
        differ=$((differ + 1));  printf '  DIFFERS          %s\n' "${rel}"
    fi
done < <(find "${LLAMA_DIR}" -type f -print0)

total=$((same + differ + absent))
[[ "${total}" -gt 0 ]] || fail_setup "walked 0 files — is the vendored tree empty?"

printf '\n%d files: %d identical, %d differing, %d not in upstream\n' \
    "${total}" "${same}" "${differ}" "${absent}"

if [[ "${differ}" -ne 0 || "${absent}" -ne 0 ]]; then
    printf '\nFAIL: the vendored tree is NOT upstream %s.\n' "${SHA}"
    printf 'Either re-extract the tree at that sha, or update LLAMA_CPP_COMMIT to\n'
    printf 'the sha it actually is. Do not leave the marker disagreeing with the tree.\n'
    exit 1
fi

printf 'OK: vendored llama.cpp is exactly upstream %s\n' "${SHA}"
