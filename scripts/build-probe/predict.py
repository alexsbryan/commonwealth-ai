#!/usr/bin/env python3
"""predict — the cone model behind a rung's pre-registered delta.

For every probe crate in quality/campaigns/build-latency.toml, the cost of an
edit is modelled as the longest path (in seconds) from that crate through its
transitive dependents, using each workspace crate's measured build duration
from cargo --timings files. `--cut A:B` deletes the edge A -> B (A depends on
B); `--split A:NAME:FRACTION` models splitting crate A so that only FRACTION
of its duration stays on the chain for NAME's dependents. Prints the weighted
chain before and after, so a rung's prediction is a number written down
before the diff (ARCH_PRINCIPLES 5, 7).

Durations: max duration per crate over the *.build.html timings in
$BUILD_PROBE_OUT/timings (default target/build-probe/timings), i.e. warm
incremental rebuild costs, not cold ones; or, when that directory holds no
HTML, the frozen $BUILD_PROBE_OUT/durations.json (the 2026-09-17 set is
committed under quality/build-probe/2026-09-17/).
"""
import glob, json, os, subprocess, sys, tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from parse_timing import load  # noqa: E402

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True).stdout.strip()
TDIR = os.environ.get("BUILD_PROBE_OUT", os.path.join(ROOT, "target/build-probe")) + "/timings"


def graph():
    m = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=ROOT, capture_output=True, text=True).stdout)
    ws = {p["name"] for p in m["packages"]}
    deps = {p["name"]: {d["name"] for d in p["dependencies"] if d["name"] in ws and d.get("kind") is None} for p in m["packages"]}
    return ws, deps


def durations():
    d = {}
    frozen = os.path.join(os.path.dirname(TDIR), "durations.json")
    if os.path.exists(frozen) and not glob.glob(TDIR + "/*.html"):
        return json.load(open(frozen))
    for f in glob.glob(TDIR + "/*.build*.html") + glob.glob(TDIR + "/*build-ws*.html"):
        for u in load(f):
            if u["mode"] == "run-custom-build" or "build-script" in u["target"]:
                continue
            d[u["name"]] = max(d.get(u["name"], 0.0), u["duration"])
    return d


def chain(crate, rev, dur, memo):
    if crate in memo:
        return memo[crate]
    best = (dur.get(crate, 0.0), [crate])
    for dep in rev.get(crate, ()):
        s, path = chain(dep, rev, dur, memo)
        if dur.get(crate, 0.0) + s > best[0]:
            best = (dur.get(crate, 0.0) + s, [crate] + path)
    memo[crate] = best
    return best


def main(argv):
    ws, deps = graph()
    cuts = [a.split(":") for a in argv if a.startswith("--cut=")]
    cuts = [(c[0].replace("--cut=", ""), c[1]) for c in cuts]
    splits = [a.replace("--split=", "").split(":") for a in argv if a.startswith("--split=")]
    dur = durations()
    if not dur:
        sys.exit(f"no timing files under {TDIR}; run score.sh --build or probe.sh first")
    probes = tomllib.load(open(os.path.join(ROOT, "quality/campaigns/build-latency.toml"), "rb"))["probe"]

    def model(deps, dur):
        rev = {n: set() for n in ws}
        for n, ds in deps.items():
            for d in ds:
                rev[d].add(n)
        memo = {}
        return {p["crate"]: chain(p["crate"], rev, dur, memo) for p in probes}

    before = model(deps, dur)
    deps2 = {k: set(v) for k, v in deps.items()}
    dur2 = dict(dur)
    for a, b in cuts:
        deps2[a].discard(b)
    for a, name, frac in splits:
        # the split-off crate NAME keeps `frac` of A's duration on the chain of A's dependents
        dur2[a] = dur[a] * float(frac)
    after = model(deps2, dur2)
    W = sum(p["weight"] for p in probes)
    wb = sum(p["weight"] * before[p["crate"]][0] for p in probes) / W
    wa = sum(p["weight"] * after[p["crate"]][0] for p in probes) / W
    print(f"{'crate':24s} {'w':>4s} {'chain now':>10s} {'after':>8s}   path now")
    for p in sorted(probes, key=lambda p: -p["weight"]):
        c = p["crate"]
        print(f"{c:24s} {p['weight']:4d} {before[c][0]:10.1f} {after[c][0]:8.1f}   {' > '.join(x.replace('sovereign-', 's-') for x in before[c][1])}")
    print(f"\nweighted chain seconds: {wb:.1f} -> {wa:.1f}  (delta {wa - wb:+.1f})   cuts={cuts} splits={splits}")


if __name__ == "__main__":
    main(sys.argv[1:])
