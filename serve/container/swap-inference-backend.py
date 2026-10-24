#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Swap the Linux llama-cpp-4 GPU backend feature to the one this image builds.

The backend choice lives inside a target-conditional dependency declaration in
serve/crates/sovereign-inference/Cargo.toml, not in the workspace [features]
table, so neither Containerfile can select it from the outside. Both therefore
rewrite that one line in-image, after the crates are copied: the CUDA image to
"cuda", the ROCm image to "hip".

    swap-inference-backend.py <containerfile-from-dir> <cuda|hip>

Tolerant of:
  - either "rocm" or "vulkan" as the source feature (the repo default has
    flipped over time);
  - comment / blank lines between the [target...] header and the llama-cpp-4
    line (the in-repo block explaining the ROCm SEGV workaround is one such).

Refuses (non-zero) when the line is missing or carries neither source token: a
swap that silently no-ops would ship an image whose daemon links the wrong
backend, which builds and then fails at first device init.
"""
import pathlib
import re
import sys

HEADER = '[target.\'cfg(target_os = "linux")\'.dependencies]'

if len(sys.argv) != 3 or sys.argv[2] not in ("cuda", "hip"):
    sys.exit("usage: swap-inference-backend.py <cargo-toml-root> <cuda|hip>")
target = sys.argv[2]

p = pathlib.Path(sys.argv[1]) / "serve/crates/sovereign-inference/Cargo.toml"
src = p.read_text()
pattern = (
    re.escape(HEADER)
    + r'(?:[ \t]*\n(?:[ \t]*#[^\n]*\n|[ \t]*\n)*)'
    + r'(llama-cpp-4\s*=\s*\{[^\n]*\})'
)
m = re.search(pattern, src)
if not m:
    sys.exit("couldn't find Linux llama-cpp-4 line under " + HEADER)
line = m.group(1)
new_line, n = re.subn(r'"(rocm|vulkan)"', f'"{target}"', line, count=1)
if n != 1:
    sys.exit(f"expected one rocm/vulkan token to swap, found {n}: {line!r}")
p.write_text(src[: m.start(1)] + new_line + src[m.end(1) :])
print("Patched:", new_line.strip())
