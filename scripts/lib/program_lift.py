#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""program-lift — does a program build, test and RUN outside this monorepo?

    scripts/program-lift.sh --sandbox <lift> [--dir <path>] [--keep]
                            [--target-dir <path>] [--set VAR=value ...]

The one decider for "this program runs alone" (phase-b-2, principle 8). It
replaced the twins `scripts/cw-rails-lift.sh` and `scripts/cw-work-lift.sh`,
which stay as wrappers. The lifts, their seeds and their RUN smokes are data
in `scripts/program-lift.toml`; which crates a program owns and which edges it
may not take are read from `quality/ARCH_LAYERS.toml` and never restated here.

THE BAR IS A PHYSICAL LIFT, NOT A CRATE-NAME COUNT. `cargo tree` cannot see a
`build.rs`, an `include_str!` that escapes the crate root, a hand-spelled
relative path, or a test that reads the repo root. So the closure is copied
FLAT to a directory OUTSIDE the repository (cargo inherits `.cargo/config.toml`
from any ancestor, so a sandbox under the repo would carry this workspace's
linker and rustflags), a root workspace is synthesised there, and it is built,
tested and run with nothing of this monorepo on the path. `.cargo/config.toml`,
`clippy.toml`, `rust-toolchain.toml` and the root `[patch]` are not copied: a
third party has none of them. cargo-hakari's `workspace-hack` is shed from
every copied manifest and the strip is printed (fp-solo-lift).

THE FOUR VERDICTS, each with its own exit, said on the last stdout line
through `scripts/lib/judgement.py` (ARCH principle 5):

    exit 0  passed           built, tested, and the RUN smoke passed
    exit 1  failed           a measured failure; the line carries "value": 0
    exit 3  could-not-judge  a precondition of the run is absent, named
    exit 4  never-ran        built and tested; no RUN smoke is declared
    exit 2  usage

A failed or passed line also carries `"value"`, so `co-lineage.py
measure_bar` (which reads rc 0 plus a value) can read the wrappers unchanged.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import tomllib
import traceback
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts" / "lib"))
from judgement import render  # noqa: E402  (scripts/lib/judgement.py)

LIFTS = REPO / "scripts" / "program-lift.toml"
EXIT = {"passed": 0, "failed": 1, "could-not-judge": 3, "never-ran": 4}
SHIM = "workspace-hack"


class Verdict(Exception):
    def __init__(self, verdict: str, reason: str):
        super().__init__(reason)
        self.verdict, self.reason = verdict, reason


def say(*a):
    print(*a, file=sys.stderr, flush=True)


def rule(title):
    say(f"── {title} ─────────────────────────────────────────────")


def glob(pattern: str, name: str) -> bool:
    """`quality/arch-layers` `wildcard_match`: `*` is the only metacharacter."""
    return re.fullmatch(re.escape(pattern).replace(r"\*", ".*"), name) is not None


def load(path: Path) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


# ── 1. the closure, judged by the map ────────────────────────────────────────

def plan(lift_id: str, lift: dict, sandbox: Path) -> tuple[list[str], list[str]]:
    ws = load(REPO / "Cargo.toml")["workspace"]
    wsdeps = ws.get("dependencies", {})
    layers = load(REPO / "quality" / "ARCH_LAYERS.toml")
    packages = {p["name"]: p for p in layers.get("package", [])}
    leaves = {leaf["name"]: leaf for leaf in layers.get("package_leaf", [])}
    forbids = layers.get("forbid", [])
    owner = {c: p for p in packages.values() for c in p["crates"]}

    pkg = packages.get(lift["package"])
    if pkg is None:
        raise Verdict("could-not-judge", f"lift `{lift_id}` names package `{lift['package']}`, which quality/ARCH_LAYERS.toml does not declare")
    seeds = lift.get("seeds", pkg["crates"])
    strays = [s for s in seeds if owner.get(s) is not pkg]
    if strays:
        raise Verdict("could-not-judge", f"lift `{lift_id}` seeds {strays}, which are not members of [[package]] `{pkg['name']}`")

    dirs = {}
    for member in ws.get("members", []):
        for d in REPO.glob(member):
            if (d / "Cargo.toml").is_file():
                dirs[load(d / "Cargo.toml")["package"]["name"]] = d

    def breach(frm: str, to: str) -> str | None:
        """The package pass of `evaluate_packages`, minus the [[exception]]
        ledger: a grandfathered edge is still an edge a lift has to carry."""
        for f in forbids:
            if glob(f["from"], frm) and glob(f["to"], to) and not any(glob(x, to) for x in f.get("except", [])):
                return f"FORBIDDEN {frm} -> {to}: [[forbid]] {f['from']} -> {f['to']}"
        p = owner.get(frm)
        if p is not None:
            allowed = set(p["crates"]) | set(p.get("leaf_budget", leaves))
            scope = f"[[package]] {p['name']}"
        elif frm in leaves:
            allowed, scope = set(leaves[frm].get("allow", [])), "its [[package_leaf]] budget"
        else:
            return None
        return None if to in allowed else f"OUTSIDE {frm} -> {to}: leaves {scope}"

    found, fails, referenced, holders = {}, [], set(), set()
    queue = [(s, dirs[s]) for s in seeds]
    while queue:
        name, cdir = queue.pop()
        if name in found:
            continue
        found[name] = cdir
        m = load(cdir / "Cargo.toml")
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            for dep, spec in m.get(table, {}).items():
                spec = spec if isinstance(spec, dict) else {"version": spec}
                if dep == SHIM:
                    holders.add(name)
                    continue
                if spec.get("workspace"):
                    referenced.add(dep)
                    entry = wsdeps.get(dep)
                    if entry is None:
                        fails.append(f"UNDECLARED {name} -> {dep}: `workspace = true` with no root entry")
                    elif isinstance(entry, dict) and "path" in entry:
                        why = breach(name, dep)
                        if why:
                            fails.append(f"{why} ({table})")
                        else:
                            queue.append((dep, REPO / entry["path"]))
                elif "path" in spec:
                    # Dies at `cargo metadata` outside this tree; boundary-gate
                    # checks that an edge is legal, never how it is spelled.
                    fails.append(f"HAND-SPELLED-PATH {name} -> {dep} = {{ path = \"{spec['path']}\" }} ({table})")
    for f in fails:
        print(f)
    if fails:
        raise Verdict("failed", f"the closure cannot leave the monorepo: {len(fails)} edge(s), first `{fails[0]}`")

    # Hazards a green `cargo tree` and a green boundary-gate both miss.
    # Reported whether or not they bite; the build below decides.
    hazard = re.compile(r'CARGO_MANIFEST_DIR|git\s+ls-files|include_(str|bytes)!\s*\(\s*"[^"]*\.\.')
    for name, cdir in sorted(found.items()):
        if (cdir / "build.rs").exists():
            print(f"HAZARD {name}: build.rs")
        for fp in sorted(cdir.rglob("*.rs")):
            if "target" in fp.relative_to(cdir).parts:
                continue
            for i, line in enumerate(open(fp, errors="replace"), 1):
                if hazard.search(line) and not line.lstrip().startswith(("//", "*")):
                    print(f"HAZARD {fp.relative_to(REPO)}:{i}: {line.strip()[:110]}")
    for dep in load(REPO / "Cargo.toml").get("patch", {}).get("crates-io", {}):
        print(f"HAZARD the root [patch.crates-io] `{dep}` is not carried; a lift resolves it from crates.io")

    (sandbox / "crates").mkdir(parents=True)
    for name, cdir in found.items():
        shutil.copytree(cdir, sandbox / "crates" / name, ignore=shutil.ignore_patterns("target", ".git"))
    shim_line = re.compile(r"^" + re.escape(SHIM) + r"\s*=")
    for name in sorted(holders):
        p = sandbox / "crates" / name / "Cargo.toml"
        lines = p.read_text().splitlines(keepends=True)
        kept = [ln for ln in lines if not shim_line.match(ln)]
        if len(kept) == len(lines) or any(SHIM in ln and not ln.lstrip().startswith("#") for ln in kept):
            raise Verdict("could-not-judge", f"{name} declares {SHIM} in a shape the planner cannot strip")
        p.write_text("".join(kept))
    print(f"STRIPPED {SHIM} from {len(holders)} manifest(s): {' '.join(sorted(holders))}")

    def render_toml(v):
        if isinstance(v, bool):
            return "true" if v else "false"
        if isinstance(v, (int, float)):
            return str(v)
        if isinstance(v, str):
            return json.dumps(v)
        if isinstance(v, list):
            return "[" + ", ".join(render_toml(x) for x in v) + "]"
        return "{ " + ", ".join(f"{k} = {render_toml(x)}" for k, x in v.items()) + " }"

    out = ["[workspace]", 'resolver = "2"', 'members = ["crates/*"]', "", "[workspace.package]"]
    out += [f"{k} = {render_toml(v)}" for k, v in ws.get("package", {}).items()]
    for section, body in ws.get("lints", {}).items():
        out += ["", f"[workspace.lints.{section}]"] + [f"{k} = {render_toml(v)}" for k, v in body.items()]
    out += ["", "[workspace.dependencies]"]
    for dep in sorted(referenced):
        entry = wsdeps[dep]
        if isinstance(entry, dict) and "path" in entry:
            entry = dict(entry, path=f"crates/{dep}")
        out.append(f"{dep} = {render_toml(entry)}")
    (sandbox / "Cargo.toml").write_text("\n".join(out) + "\n")
    # The lock travels, as `cargo package` would give a third party one.
    shutil.copyfile(REPO / "Cargo.lock", sandbox / "Cargo.lock")
    print("CRATES " + " ".join(sorted(found)))
    print(f"COUNT {len(found)}")
    return seeds, sorted(found)


# ── 2-4. resolve, build, test ────────────────────────────────────────────────

def cargo(args: list[str], sandbox: Path, target: Path, log: str, show: str) -> tuple[int, float]:
    # RUSTC_WRAPPER cleared: sccache would report a cold build as seconds of nothing.
    env = dict(os.environ, RUSTC_WRAPPER="", CARGO_TARGET_DIR=str(target))
    t0 = time.monotonic()
    with open(sandbox / log, "w") as f:
        p = subprocess.Popen(["cargo", *args], cwd=sandbox, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in p.stdout:
            f.write(line)
            if re.match(show, line):
                say(line.rstrip())
        rc = p.wait()
    return rc, time.monotonic() - t0


def seeds_args(crates: list[str]) -> list[str]:
    return [a for c in crates for a in ("-p", c)]


# ── 5. the RUN smoke, interpreted from data ──────────────────────────────────

VAR = re.compile(r"\$\{([A-Z_][A-Z0-9_]*)\}")
SEG = re.compile(r"^([A-Za-z_]\w*)(?:\[(\w+)(!?=)([^\]]*)\])?$")


def walk(doc, path: str) -> list:
    cur = [doc]
    for seg in path.split("."):
        name, key, op, want = SEG.match(seg).groups()
        nxt = []
        for c in cur:
            if not isinstance(c, dict) or name not in c:
                continue
            items = c[name] if isinstance(c[name], list) else [c[name]]
            if key:
                items = [i for i in items if isinstance(i, dict)
                         and (str(i.get(key)).lower() == want.lower()) == (op == "=")]
            nxt.extend(items)
        cur = nxt
    return [c for c in cur if c not in (None, "")]


def http(method: str, url: str, body: dict | None = None, timeout: float = 5.0) -> tuple[int, str]:
    """(status, body); status 0 is no answer. A refusal keeps its body."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={"content-type": "application/json"} if data else {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")
    except (urllib.error.URLError, OSError, ValueError):
        return 0, ""


class Smoke:
    def __init__(self, sandbox: Path, variables: dict, keep: bool):
        self.sandbox, self.v, self.keep = sandbox, variables, keep
        self.docs, self.procs, self.notes = {}, {}, []

    def sub(self, x):
        if isinstance(x, str):
            return VAR.sub(lambda m: str(self.v.get(m.group(1), "")), x)
        if isinstance(x, list):
            return [self.sub(i) for i in x]
        if isinstance(x, dict):
            return {k: self.sub(i) for k, i in x.items()}
        return x

    def stop(self, s: dict, key: str, default_verdict: str = "failed"):
        """End the run with the step's own message: `abstain` wins over `fail`."""
        if key == "abstain" or (key == "fail" and s.get("on_error") == "abstain"):
            raise Verdict("could-not-judge", self.sub(s.get("abstain") or s["fail"]))
        raise Verdict(default_verdict, self.sub(s[key]))

    def json_at(self, text: str, path: str) -> list:
        try:
            return walk(json.loads(text), self.sub(path))
        except (json.JSONDecodeError, TypeError):
            return []

    def check_alive(self, s: dict):
        p = self.procs.get(s.get("alive"))
        if p is not None and p.poll() is not None:
            raise Verdict("failed", f"`{s['alive']}` exited (rc {p.returncode}) while the run waited on it; see {self.sandbox}/{s['alive']}.log")

    def run(self, steps: list[dict]):
        for s in steps:
            getattr(self, "do_" + s["do"])(s)

    def do_input(self, s):
        val = os.environ.get(s["env"], "") if "env" in s else ""
        if not val and "get" in s:
            status, body = http("GET", self.sub(s["get"]))
            val = (self.json_at(body, s["path"]) or [""])[0] if status == 200 else ""
        val = val or self.sub(s.get("value", "")) or self.sub(s.get("default", ""))
        if "strip" in s and val.endswith(s["strip"]):
            val = val[: -len(s["strip"])]
        if not val and "abstain" in s:
            self.stop(s, "abstain")
        self.v[s["var"]] = str(val)

    def do_require(self, s):
        if not re.search(s["match"], self.sub(s["value"])):
            self.stop(s, "abstain" if "abstain" in s else "fail")

    def do_unexpired(self, s):
        m = re.search(r"[?&]" + re.escape(s["param"]) + r"=(\d+)", self.sub(s["value"]))
        if m and int(m.group(1)) < time.time():
            self.stop(s, "abstain")

    def do_ports(self, s):
        for name in s["vars"]:
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                self.v[name] = str(sock.getsockname()[1])

    def do_write(self, s):
        p = Path(self.sub(s["path"]))
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(self.sub(s["text"]))

    def _popen(self, s, log):
        env = dict(os.environ, **self.sub(s.get("env", {})))
        return subprocess.Popen(self.sub(s["argv"]), cwd=self.sub(s.get("cwd", str(self.sandbox))), env=env,
                                stdout=open(self.sandbox / log, "w"), stderr=subprocess.STDOUT, start_new_session=True)

    def do_spawn(self, s):
        self.procs[s["id"]] = self._popen(s, s.get("log", s["id"] + ".log"))

    def do_exec(self, s):
        rc = self._popen(s, s["log"]).wait()
        say(f"{' '.join(self.sub(s['argv']))[:90]}: rc={rc}")
        if s.get("expect") == "fail":
            if rc == 0:
                self.stop(s, "abstain")
        elif rc == 3:
            raise Verdict("could-not-judge", f"`{s['argv'][0]}` could not judge its own run (rc 3); see {self.sandbox}/{s['log']}")
        elif rc != 0:
            self.stop(s, "fail")

    def do_exists(self, s):
        if not Path(self.sub(s["path"])).exists():
            self.stop(s, "fail")

    def do_poll(self, s):
        url, body = self.sub(s["url"]), ""
        for _ in range(int(s.get("timeout", 60))):
            self.check_alive(s)
            status, body = http("GET", url)
            hits = self.json_at(body, s["path"]) if status == 200 else []
            if hits:
                self.v[s["var"]], self.docs[s.get("doc", s["var"])] = str(hits[0]), json.loads(body)
                say(f"{url} -> {s['var']}={hits[0]}")
                return
            time.sleep(1)
        second = s.get("second")
        if second is None:
            self.stop(s, "fail")
        status, body = http("GET", self.sub(second["url"]))
        hits = self.json_at(body, second["path"]) if status == 200 else []
        self.v["SECOND"] = ", ".join(map(str, hits))
        self.stop(second, "fail" if hits else "abstain")

    def do_select(self, s):
        doc = self.docs.get(s["doc"], {})
        if "all" in s:
            self.v[s["all_var"]] = ", ".join(map(str, walk(doc, self.sub(s["all"]))))
        hits = walk(doc, self.sub(s["path"]))
        if not hits:
            self.stop(s, "abstain")
        self.v[s["var"]] = str(hits[0])

    def do_get(self, s):
        status, body = http("GET", self.sub(s["url"]), timeout=float(s.get("timeout", 10)))
        self.v["BODY"] = body.strip()[:300]
        if "status_var" in s:
            self.v[s["status_var"]] = str(status)
        if status == 0:
            self.stop(s, "fail")
        if "path" in s:
            hits = self.json_at(body, s["path"]) if 200 <= status < 300 else []
            if not hits:
                self.stop(s, "fail")
            self.v[s["var"]] = str(hits[0])
        say(f"GET {self.sub(s['url'])} -> HTTP {status}")

    def do_count(self, s):
        try:
            n = sum(1 for ln in open(self.sub(s["file"]), errors="replace") if re.search(s["pattern"], ln))
        except OSError:
            n = 0
        self.v[s["var"]] = str(n)
        if n < int(s["min"]):
            self.stop(s, "fail")

    # finally: never raises; each outcome is a note beside the verdict.
    def do_post(self, s):
        missing = [n for n in s.get("needs", "").split() if not self.v.get(n)]
        if missing:
            self.notes.append(f"{s['label']}: not attempted, {' '.join(missing)} never set")
            return
        url = self.sub(s["url"])
        status, body = http("POST", url, self.sub(s["json"]), timeout=30)
        self.notes.append(f"{s['label']}: POST {url} -> HTTP {status} {body.strip()[:200]}")

    def do_remove(self, s):
        if not self.keep:
            for p in self.sub(s["paths"]):
                shutil.rmtree(p, ignore_errors=True)

    def teardown(self, final: list[dict]):
        for p in self.procs.values():
            if p.poll() is None:
                try:
                    os.killpg(p.pid, 15)
                    p.wait(timeout=10)
                except (ProcessLookupError, subprocess.TimeoutExpired):
                    os.killpg(p.pid, 9)
        for s in final:
            getattr(self, "do_" + s["do"])(s)
        for n in self.notes:
            say(n)


# ── the run ──────────────────────────────────────────────────────────────────

def lift(lift_id: str, spec: dict, sandbox: Path, target: Path, keep: bool, sets: dict, record: dict) -> tuple[str, str]:
    rule("1. the closure, judged by quality/ARCH_LAYERS.toml")
    seeds, record["crates"] = plan(lift_id, spec, sandbox)
    say(f"toolchain: {subprocess.run(['cargo', '--version'], cwd=sandbox, capture_output=True, text=True).stdout.strip()}")
    say(f"sandbox: {sandbox}  (repo is {REPO}; nothing under it is on this path)")

    rule("2. does it resolve at all?")
    # A registry that cannot be reached says nothing about the closure.
    r = subprocess.run(["cargo", "metadata", "--format-version", "1"], cwd=sandbox, capture_output=True, text=True,
                       env=dict(os.environ, RUSTC_WRAPPER=""))
    if r.returncode != 0:
        say(r.stderr[-2000:])
        raise Verdict("could-not-judge", "the sandbox workspace would not resolve; see the cargo output above")
    record["resolved_packages"] = len(json.loads(r.stdout)["packages"])
    say(f"resolved packages in the lifted closure: {record['resolved_packages']}")

    rule("3. the build, outside the monorepo")
    rc, secs = cargo(["build", *spec.get("build", seeds_args(seeds))], sandbox, target, "build.log", r"^(error|warning: unused)")
    record["build_s"] = round(secs, 1)
    say(f"build: rc={rc} in {secs:.1f}s")
    if rc != 0:
        raise Verdict("failed", f"the closure does not BUILD outside this monorepo ({secs:.1f}s, see {sandbox}/build.log)")
    binary = target / spec["binary"] if "binary" in spec else None
    if binary is not None and not binary.exists():
        raise Verdict("failed", f"the build reported success and produced no {binary}")

    rule("4. the package's own tests, in isolation")
    rc, _ = cargo(["test", *spec.get("test", seeds_args(seeds))], sandbox, target, "test.log", r"^(error|test result|failures:)")
    say(f"test: rc={rc}")
    if rc != 0:
        raise Verdict("failed", f"the lifted closure's own tests do not pass in isolation (see {sandbox}/test.log)")
    built = f"built in {record['build_s']}s and tested outside the monorepo"

    run = spec.get("run")
    if run is None:
        raise Verdict("never-ran", f"{built}; no RUN smoke is declared for `{lift_id}` in scripts/program-lift.toml, so nothing proves it runs alone")
    rule("5. the RUN smoke")
    smoke = Smoke(sandbox, dict(SANDBOX=str(sandbox), TARGET=str(target), BIN=str(binary or ""), **sets), keep)
    try:
        smoke.run(run["steps"])
        return "passed", f"{built}, and RAN: {smoke.sub(run['pass'])}"
    finally:
        smoke.teardown(run.get("finally", []))
        record["finally"] = smoke.notes


def main(argv: list[str]) -> int:
    usage = "usage: scripts/program-lift.sh --sandbox <lift> [--dir <path>] [--keep] [--target-dir <path>] [--set VAR=value ...]"
    lifts = load(LIFTS)["lift"]
    mode, lift_id, sandbox, target, keep, sets = None, None, None, None, False, {}
    it = iter(argv)
    for a in it:
        if a == "--sandbox":
            mode = "sandbox"
        elif a == "--keep":
            keep = True
        elif a in ("--dir", "--target-dir", "--set"):
            val = next(it, "")
            if a == "--dir":
                sandbox = Path(val)
            elif a == "--target-dir":
                target = Path(val)
            else:
                k, _, v = val.partition("=")
                sets[k] = v
        elif a in ("-h", "--help"):
            say(usage + "\nlifts: " + " ".join(lifts))
            return 2
        elif lift_id is None and not a.startswith("-"):
            lift_id = a
        else:
            say(f"unknown argument `{a}`\n{usage}")
            return 2
    if mode != "sandbox" or lift_id not in lifts:
        say(f"{usage}\nlifts: {' '.join(lifts)}")
        return 2

    sandbox = (sandbox or Path(os.environ.get("TMPDIR", "/tmp")) / f"program-lift.{lift_id}.{os.getpid()}").resolve()
    if sandbox == REPO or REPO in sandbox.parents:
        say(f"refusing a sandbox inside the repository: {sandbox}")
        return 2
    target = (target or sandbox / "target").resolve()
    artifact = REPO / "target" / "program-lift" / lift_id / "last.json"
    record = {"lift": lift_id, "sandbox": str(sandbox), "target_dir": str(target), "sets": sorted(sets)}

    try:
        if shutil.which("cargo") is None:
            raise Verdict("could-not-judge", "cargo is not on PATH, so nothing could be built")
        shutil.rmtree(sandbox, ignore_errors=True)
        sandbox.mkdir(parents=True)
        verdict, reason = lift(lift_id, lifts[lift_id], sandbox, target, keep, sets, record)
    except Verdict as v:
        verdict, reason = v.verdict, v.reason
    except Exception as e:  # noqa: BLE001
        # A crash is the instrument not judging, never a measured failure: a
        # full disk read as exit 1 on 2026-09-25 (ingest, tmpfs /tmp at quota).
        traceback.print_exc()
        verdict, reason = "could-not-judge", f"the instrument stopped before a verdict: {type(e).__name__}: {e}"
    finally:
        if not keep:
            shutil.rmtree(sandbox, ignore_errors=True)
        else:
            say(f"sandbox kept at {sandbox}")
    if record.get("finally"):
        reason += "; " + "; ".join(record["finally"])

    line = json.loads(render(f"program-lift:{lift_id}", verdict, reason, time.time()))
    if verdict in ("passed", "failed"):
        line["value"] = 1 if verdict == "passed" else 0
    line["artifact"] = str(artifact.relative_to(REPO))
    artifact.parent.mkdir(parents=True, exist_ok=True)
    artifact.write_text(json.dumps(dict(record, verdict=verdict, reason=reason, at=line["as_of"]), indent=2) + "\n")
    say(f"\n{verdict.upper()} — {reason}")
    print(json.dumps(line, separators=(",", ":")))
    return EXIT[verdict]


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
