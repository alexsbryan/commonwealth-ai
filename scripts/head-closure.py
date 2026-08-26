#!/usr/bin/env python3
"""head-closure — does a COMMIT reference a path that commit does not contain?

Reads ONLY the git object store, never the working tree, so the verdict is
about the COMMIT. That immunity is the whole point: in a shared tree a build
is evidence about neither direction — it can hide a real HEAD breakage behind
an untracked file, and it can invent one from a peer's uncommitted edit.

Catches: `mod x;` whose file is absent, and include_str!/include_bytes! whose
target is absent. Both are "the commit is missing a file its code needs".

KNOWN LIMITS (stated so nobody mistakes a pass for more than it is):
  - a `mod x;` nested inside an inline `mod y { .. }` block resolves relative
    to y, which this does not model; such a decl may false-positive.
  - path attribute overrides are not honoured (this repo has none: verified
    with a cached grep -> 0 files).
  - include_* with a `concat!`/macro argument is invisible.
  It is a closure check, not a build. A pass does not mean the commit compiles.
"""
import re, subprocess, sys, posixpath

def git(*a):
    return subprocess.run(("git",) + a, capture_output=True, text=True).stdout

def strip(src):
    """Remove block comments, line comments, and byte/raw string bodies."""
    src = re.sub(r'/\*.*?\*/', '', src, flags=re.S)
    return re.sub(r'//[^\n]*', '', src)

REF = sys.argv[1] if len(sys.argv) > 1 else "HEAD"
tree = set(git("ls-tree", "-r", "--name-only", REF).split("\n")) - {""}
if not tree:
    sys.exit(f"head-closure: {REF} has no tree — bad ref?")
rs = [f for f in tree if f.endswith(".rs")]
MOD = re.compile(r'^[ \t]{0,4}(?:pub\s+|pub\([^)]*\)\s+)?mod\s+([A-Za-z_]\w*)\s*;', re.M)
INC = re.compile(r'include_(?:str|bytes)!\s*\(\s*"([^"]+)"')

bad = []
for f in rs:
    src = strip(git("show", f"{REF}:{f}"))
    d = posixpath.dirname(f)
    base = posixpath.basename(f)
    # Rust module resolution. Children sit in the SAME dir when the file is a
    # module root (`mod.rs`) or a CRATE root; otherwise in `foo/` beside it.
    # Crate roots include every file cargo auto-discovers as its own target:
    # tests/*.rs, benches/*.rs, examples/*.rs and src/bin/*.rs are each a
    # separate binary, which is why `tests/a.rs` and `tests/b.rs` can both say
    # `mod common;` and mean `tests/common.rs`. Missing this rule produced 32
    # false positives on first run.
    parent = posixpath.basename(d)
    is_crate_root = (
        base in ("mod.rs", "lib.rs", "main.rs", "build.rs")
        or parent in ("tests", "benches", "examples", "bin")
    )
    childdir = d if is_crate_root else f"{d}/{base[:-3]}"
    for m in MOD.finditer(src):
        n = m.group(1)
        if not ({f"{childdir}/{n}.rs", f"{childdir}/{n}/mod.rs"} & tree):
            bad.append((f, f"mod {n};", f"{childdir}/{n}.rs"))
    for m in INC.finditer(src):
        p = posixpath.normpath(posixpath.join(d, m.group(1)))
        if p not in tree:
            bad.append((f, f'include_*!("{m.group(1)}")', p))

if bad:
    print(f"head-closure: FAIL — {len(bad)} reference(s) in {REF} to path(s) it does not contain\n")
    for f, what, want in bad:
        print(f"  {f}\n      {what}  ->  missing: {want}")
    sys.exit(1)
print(f"head-closure: PASS — {REF} is closed. Every `mod` and include_* resolves "
      f"inside the commit ({len(rs)} rust files, {len(tree)} tracked paths).")
