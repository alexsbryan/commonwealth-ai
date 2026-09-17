#!/usr/bin/env python3
"""scope-drift — bl-scope-drift instrument (quality/campaigns/build-latency.toml).

For each probe crate, resolve the workspace's features twice with `cargo tree`
(metadata only, no build): once for `--workspace` under the full gate feature
list, once for `-p <crate>` under the scope-aware list sovereign-test.sh would
use. Print the number of packages the scoped run resolves with a feature set the
workspace run never compiles (a unit that exists only under that scope),
per crate and summed. 0 means a scoped run and a workspace run compile the
same units, so the first scoped run in a new scope costs what the edit costs
and nothing more (BUILD_LATENCY.md D1). Exit 0 iff the sum is 0.
"""
import os, re, subprocess, sys, tomllib

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True).stdout.strip()
FULL = "corpus-engine/treesitter,sovereign-cli/dev-tools,sovereign-cli/code-intel,sovereign-cli/awareness,sovereign-mesh/mesh-sim,sovereign-mesh/dst,sovereign-turn-client/bundled-backend"


def resolved(args):
    out = subprocess.run(["cargo", "tree", "--offline", "-e", "normal,build,dev", "-f", "{p}|{f}", *args],
                         cwd=ROOT, capture_output=True, text=True).stdout
    d = {}
    for line in out.splitlines():
        line = re.sub(r"^[^A-Za-z]*", "", line)
        if "|" not in line:
            continue
        p, f = line.split("|", 1)
        name, _, ver = p.partition(" ")
        d.setdefault(f"{name}@{ver.split(' ')[0]}", set()).add(f.replace(" (*)", ""))
    return d


def scope_features(crate):
    r = subprocess.run(["bash", "-c", f'source scripts/lib/cargo-scope.sh; resolve_features {crate}'],
                       cwd=ROOT, capture_output=True, text=True, env={**os.environ, "REPO_ROOT": ROOT})
    return r.stdout.strip().splitlines()[-1] if r.stdout.strip() else ""


def main():
    cfg = tomllib.load(open(os.path.join(ROOT, "quality/campaigns/build-latency.toml"), "rb"))
    crates = sorted({p["crate"] for p in cfg["probe"]})
    full = resolved(["--workspace", "--features", FULL])
    total = 0
    for c in crates:
        feats = scope_features(c)
        scoped = resolved(["-p", c] + (["--features", feats] if feats else []))
        # A package may legitimately carry two feature sets in the full run
        # (host/build-dep instance vs target instance); drift is a scoped
        # instance the full run never compiles, not a missing duplicate.
        diff = [p for p in scoped if p in full and not scoped[p] <= full[p]]
        total += len(diff)
        print(f"{c:28s} {len(diff):4d}   " + (", ".join(sorted(p.split('@')[0] for p in diff)[:8]) if diff else ""))
    print(f"{'TOTAL':28s} {total:4d}")
    return 0 if total == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
