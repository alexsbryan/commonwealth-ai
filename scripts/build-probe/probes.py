#!/usr/bin/env python3
"""probes — build the [[probe]] rows for quality/campaigns/build-latency.toml
mechanically, so which files are on the ruler is not anyone's choice.

Rank workspace crates by `.rs` file-edits in [--since, --until]; take crates
in that order until --coverage of all file-edits is reached; for each, pick
the most-edited file under src/ that has a function body the score script
can insert a statement into (fall back to tests/), and the first
`#[test]`/`#[tokio::test]` function in the crate as the focused test (a crate
with none gets shapes = ["lint"]). Rows already present in the toml (same
file) are kept verbatim so a snapshot taken against them stays comparable.

    python3 scripts/build-probe/probes.py --since 2026-08-18 --until 2026-09-17 --coverage 0.92 [--write]
"""
import collections, json, os, re, subprocess, sys, tomllib

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True).stdout.strip()
TOML = os.path.join(ROOT, "quality/campaigns/build-latency.toml")
FN = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?(async\s+)?(unsafe\s+)?fn\s+[a-z_0-9]+[^;]*\{\s*$")
TEST = re.compile(r"#\[(tokio::)?test[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+([a-z_0-9]+)")


def arg(name, default):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def main():
    since, until, cov = arg("--since", "2026-08-18"), arg("--until", "2026-09-17"), float(arg("--coverage", "0.92"))
    meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, capture_output=True, text=True).stdout)
    crate_dir = {p["name"]: os.path.relpath(os.path.dirname(p["manifest_path"]), ROOT) for p in meta["packages"]}
    dir_crate = sorted(crate_dir.items(), key=lambda kv: -len(kv[1]))
    log = subprocess.run(["git", "log", f"--since={since}", f"--until={until}", "--name-only", "--format=", "--", "*.rs"], cwd=ROOT, capture_output=True, text=True).stdout
    edits, files = collections.Counter(), collections.defaultdict(collections.Counter)
    for line in log.splitlines():
        p = line.strip()
        if not p:
            continue
        for name, d in dir_crate:
            if p.startswith(d + "/"):
                edits[name] += 1
                files[name][p] += 1
                break
    total = sum(edits.values())
    existing = {p["file"]: p for p in tomllib.load(open(TOML, "rb"))["probe"]}
    rows, cum = [], 0
    for name, n in edits.most_common():
        if cum / total >= cov:
            break
        cum += n
        pinned = [p for p in existing.values() if p["crate"] == name]
        if pinned:
            rows.extend(pinned)
            continue
        chosen = None
        for f, _ in files[name].most_common():
            if not os.path.exists(os.path.join(ROOT, f)):
                continue
            if "/src/" in f or f.startswith(crate_dir[name] + "/src/"):
                if any(FN.match(l) for l in open(os.path.join(ROOT, f), errors="replace")):
                    chosen = f
                    break
        if chosen is None:
            for f, _ in files[name].most_common():
                if os.path.exists(os.path.join(ROOT, f)) and any(FN.match(l) for l in open(os.path.join(ROOT, f), errors="replace")):
                    chosen = f
                    break
        if chosen is None:
            print(f"# {name}: no editable file found ({n} edits) — skipped", file=sys.stderr)
            continue
        test = None
        for sub in ("src", "tests"):
            d = os.path.join(ROOT, crate_dir[name], sub)
            for dp, _, fs in os.walk(d):
                for fn in sorted(fs):
                    if fn.endswith(".rs"):
                        m = TEST.search(open(os.path.join(dp, fn), errors="replace").read())
                        if m:
                            test = m.group(2)
                            break
                if test:
                    break
            if test:
                break
        row = {"file": chosen, "crate": name, "weight": n, "test": test or ""}
        if not test:
            row["shapes"] = ["lint"]
        elif "/tests/" in chosen:
            row["shapes"] = ["test"]
        rows.append(row)
    out = []
    for r in rows:
        out.append(f'[[probe]]\nfile   = "{r["file"]}"\ncrate  = "{r["crate"]}"\nweight = {r["weight"]}\ntest   = "{r["test"]}"' + (f'\nshapes = {json.dumps(r["shapes"])}' if r.get("shapes") else "") + "\n")
    text = "\n".join(out)
    print(f"# {len(rows)} probes over {len({r['crate'] for r in rows})} crates cover {cum/total*100:.0f}% of {total} .rs file-edits {since}..{until}", file=sys.stderr)
    if "--write" in sys.argv:
        t = open(TOML).read()
        s, e = t.index("[[probe]]"), t.index("# ── SUCCESS")
        open(TOML, "w").write(t[:s] + text + "\n" + t[e:])
        print("written", file=sys.stderr)
    else:
        print(text)


if __name__ == "__main__":
    main()
