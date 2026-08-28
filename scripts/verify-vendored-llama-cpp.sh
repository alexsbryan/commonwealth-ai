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
# TWO DIRECTIONS, because one of them was not enough. Until 2026-08-28 this
# script walked only the vendored tree, and its own comment said "a file
# present upstream but not here is expected and is not reported". That makes
# an EDIT visible and an OMISSION invisible — and the failure that actually
# shipped was an omission. The 035e22731a fast-forward moved the Metal
# shaders to `ggml/src/ggml-metal/kernels/*.metal`; the vendoring step copies
# files that already exist, so 20 new upstream files were never taken. This
# script reported "1764 files: 1764 identical, 0 differing, 0 not in
# upstream" and exit 0, while cmake on macOS could not configure at all
# (`file STRINGS file .../kernels/fa.metal cannot be read`). Linux builds
# GGML_METAL=OFF and never saw it.
#
# So: forward pass = nothing was edited. Reverse pass = nothing is missing,
# for every directory we vendor from, modulo the deliberate exclusions in
# vendor/llama-cpp-sys-4/VENDOR_EXCLUDE. The reverse pass is scoped to
# directories we already vendor because the published crate ships a subset of
# upstream on purpose; a whole directory we take nothing from is not an
# omission, but a hole INSIDE one we do take from is.
#
# Exit codes:
#   0  every vendored file matches upstream, and none are missing
#   1  a file differs, is absent upstream, or is missing locally
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

EXCLUDE_FILE="${VENDOR_DIR}/VENDOR_EXCLUDE"
[[ -f "${EXCLUDE_FILE}" ]] || fail_setup "no exclusion list at ${EXCLUDE_FILE} — the reverse pass cannot run without one, and skipping it is how the last omission shipped"

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

# ── Reverse pass: what does upstream ship that we did not take? ────────────
#
# Scoped to directories we already vendor at least one file from. Excluded
# patterns are the declared, intentional omissions.
# Read with a while-loop, not `mapfile`: macOS ships bash 3.2, which has no
# `mapfile`, and this script must run on every host that builds this tree.
EXCLUDES=()
while IFS= read -r line; do
    case "${line}" in ''|'#'*) continue ;; esac
    EXCLUDES+=("${line}")
done < "${EXCLUDE_FILE}"
[[ "${#EXCLUDES[@]}" -gt 0 ]] || fail_setup "${EXCLUDE_FILE} declares no patterns"

is_excluded() {
    local rel="$1" pat
    for pat in "${EXCLUDES[@]}"; do
        # shellcheck disable=SC2254  # the pattern IS a glob, deliberately
        case "${rel}" in ${pat}) return 0 ;; esac
    done
    return 1
}

# Every directory the vendored tree has a file in, upstream-relative.
# `find -printf` is GNU-only and this runs on macOS too, so derive it from
# -print0 + dirname rather than from a format string.
VENDORED_DIRS=()
while IFS= read -r d; do
    VENDORED_DIRS+=("${d}")
done < <(find "${LLAMA_DIR}" -type f -print0 \
    | xargs -0 -n1 dirname | sort -u | sed "s#^${LLAMA_DIR}##; s#^/##")

missing=0
for d in "${VENDORED_DIRS[@]}"; do
    updir="${WORK}/upstream/${d}"
    [[ -d "${updir}" ]] || continue
    while IFS= read -r -d '' upfile; do
        rel="${upfile#"${WORK}/upstream/"}"
        [[ -f "${LLAMA_DIR}/${rel}" ]] && continue
        is_excluded "${rel}" && continue
        missing=$((missing + 1)); printf '  MISSING-LOCALLY  %s\n' "${rel}"
    done < <(find "${updir}" -maxdepth 1 -type f -print0)
done

printf '\n%d files: %d identical, %d differing, %d not in upstream, %d missing locally\n' \
    "${total}" "${same}" "${differ}" "${absent}" "${missing}"

if [[ "${differ}" -ne 0 || "${absent}" -ne 0 ]]; then
    printf '\nFAIL: the vendored tree is NOT upstream %s.\n' "${SHA}"
    printf 'Either re-extract the tree at that sha, or update LLAMA_CPP_COMMIT to\n'
    printf 'the sha it actually is. Do not leave the marker disagreeing with the tree.\n'
    exit 1
fi

if [[ "${missing}" -ne 0 ]]; then
    printf '\nFAIL: upstream %s ships %d file(s) this tree does not have, inside\n' "${SHA}" "${missing}"
    printf 'directories it DOES vendor from. A fast-forward that only refreshes files\n'
    printf 'that already exist silently drops every file upstream added — which is how\n'
    printf 'the Metal kernels went missing and the macOS build stopped configuring.\n'
    printf 'Either vendor them, or declare them in %s.\n' "${EXCLUDE_FILE#"${REPO_ROOT}"/}"
    exit 1
fi

printf 'OK: vendored llama.cpp is exactly upstream %s (both directions)\n' "${SHA}"
