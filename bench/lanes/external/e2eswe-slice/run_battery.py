#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = ["mini-swe-agent==2.4.6", "pyyaml"]
# ///
"""E2E-SWE hard-slice battery driver — bench/lanes/external/e2eswe-slice/.

Runs the E2E-SWE authors' own mini-swe-agent scaffold (their
`agents/e2e_swe_mini_sweagent.yaml`, verbatim, manifest-hash-checked) against
an OpenAI-compatible endpoint, with every bash command crossing a bwrap
sandbox that maps the workdir to /app (their contract) and has NO network
(their protocol). Verification is their contract too: fresh copy of the
workdir, `bash ./setup.sh`, then pytest — with `--junitxml` in place of their
`--ctrf` (plugin absent from PyPI; reward definition unchanged, see README).

Subcommands:
  setup   [--tasks a,b] [--skip-redis]   venvs + deps + redis + offline probes
  probe   --url URL [--arm-log PATH]     endpoint health + thinking-knob proof
  run     --task T --model M [--wall-min 80]   one agent run (needs OPENAI_API_BASE)
  score   --run-id R                     fresh-copy verify + results row

Exit statuses a run can take (kept distinct on purpose, ARCH §6):
  submitted | step_limit | error | wall_cap (process exit 124) | never_ran
"""

import argparse
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time
import tomllib
from pathlib import Path

LANE = Path(__file__).resolve().parent
OUT = Path(os.environ.get("E2ESWE_TARGET", LANE / "../../../../target/e2eswe-slice")).resolve()
REDIS_VERSION = "8.10.2"
REDIS_BIN = OUT / "bin" / "redis-server"
SUBMIT = "COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT"
TASKS = ("ldaptor", "cement", "j1939", "hojichar")
RESULTS = OUT / "results.jsonl"

UPSTREAM_CONFIG = (OUT / "upstream"
                   / f"E2E-SWE-{json.loads((LANE / 'manifest.json').read_text())['upstream_commit']}"
                   / "agents" / "e2e_swe_mini_sweagent.yaml")


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def load_task(tid):
    import re

    raw = (LANE / "tasks" / f"{tid}.toml").read_bytes()
    t = tomllib.loads(raw.decode())
    return t


def env_json():
    return json.loads((OUT / "env.json").read_text())


def scrubbed_env(venv):
    """Minimal environment for sandbox processes (Sandbox::scrubbed_env idea):
    no credentials, no proxies — the sandbox is the boundary, not the env."""
    return {
        "PATH": f"{venv}/bin:/usr/bin:/bin",
        "HOME": "/tmp",
        "LANG": "C.UTF-8",
        "VIRTUAL_ENV": str(venv),
        "PIP_NO_INDEX": "1",
        "PIP_DISABLE_PIP_VERSION_CHECK": "1",
    }


class BwrapEnvironment:
    """mini-swe-agent environment duck type (execute / get_template_vars /
    serialize — the protocol their own Harbor adapter documents) that runs every
    command under bwrap: no network, no pids beyond the command, workdir at
    /app. The venv and its base interpreter are bound at their host paths so
    console-script shebangs keep working; the venv is writable so setup.sh's
    offline `pip install -e .` lands in it."""

    def __init__(self, app, venv, base_prefix, needs_redis=False, timeout=300,
                 extra_binds=(), python_root=None):
        self.app = Path(app).resolve()
        self.venv = Path(venv).resolve()
        self.base_prefix = Path(base_prefix).resolve()
        # uv's venv symlinks point at a versionless alias dir (e.g.
        # .../uv/python/cpython-3.12-linux-x86_64-gnu) that differs from
        # sys.base_prefix (the resolved cpython-3.12.13-... dir); BOTH sides
        # of that symlink must be inside the sandbox or venv/bin/python dangles
        # and PATH silently falls through to the system python.
        self.python_root = Path(python_root).resolve() if python_root else None
        self.needs_redis = needs_redis
        # extra_binds: (host, guest, rw) triples — e.g. the verify phase binds
        # the run dir at /logs (rw, for setup.log/junit.xml) and the task's
        # hidden tests at /tests (ro), mirroring the upstream container layout.
        self.extra_binds = [(Path(a).resolve(), b, rw) for a, b, rw in extra_binds]
        self.config = type("Cfg", (), {"cwd": "/app", "timeout": timeout, "env": {}})()

    def bwrap_argv(self, inner):
        argv = [
            "bwrap",
            "--unshare-net", "--unshare-pid", "--unshare-ipc",
            "--dev", "/dev", "--proc", "/proc",
            "--ro-bind", "/usr", "/usr",
            "--ro-bind-try", "/etc", "/etc",
            "--symlink", "usr/bin", "/bin",
            "--symlink", "usr/lib", "/lib",
            "--symlink", "usr/lib64", "/lib64",
            "--tmpfs", "/tmp", "--tmpfs", "/run", "--tmpfs", "/var", "--tmpfs", "/home",
            "--bind", str(self.app), "/app",
            "--bind", str(self.venv), str(self.venv),
            "--ro-bind", str(self.base_prefix), str(self.base_prefix),
            "--chdir", "/app",
        ]
        if self.python_root:
            argv += ["--ro-bind", str(self.python_root), str(self.python_root)]
        for host, guest, rw in self.extra_binds:
            argv += ["--bind" if rw else "--ro-bind", str(host), guest]
        if self.needs_redis and REDIS_BIN.exists():
            # /usr is ro-bound, so the binary goes on the tmpfs /tmp; the
            # preamble calls it by explicit path (PATH is venv:/usr/bin:/bin).
            argv += ["--ro-bind", str(REDIS_BIN), "/tmp/redis-server"]
        argv += ["/bin/sh", "-c", inner]
        return argv

    def _inner(self, command):
        if self.needs_redis and REDIS_BIN.exists():
            # shlex.quote, not json.dumps: sh -c must receive real newlines.
            # No `exec`: redis is backgrounded under this shell and must be
            # KILLED before the shell exits — under --unshare-pid an orphaned
            # redis reparents to bwrap-as-init, which waits for it, and the
            # capture pipes never close (watched: every command timed out).
            return (
                "/tmp/redis-server --port 6379 --bind 127.0.0.1 --save '' "
                "--appendonly no --dir /tmp >/tmp/redis.log 2>&1 & RPID=$!; "
                "sleep 0.3; sh -c " + shlex.quote(command) +
                "; RC=$?; kill $RPID 2>/dev/null; wait $RPID 2>/dev/null; exit $RC"
            )
        return command

    def execute(self, action, cwd="", *, timeout=None):
        command = action.get("command", "")
        timeout = timeout or self.config.timeout
        try:
            proc = subprocess.run(
                self.bwrap_argv(self._inner(command)),
                env=scrubbed_env(self.venv),
                capture_output=True, text=True, timeout=timeout + 30,
            )
            output = (proc.stdout or "") + (proc.stderr or "")
            observation = {"output": output, "returncode": proc.returncode, "exception_info": ""}
        except subprocess.TimeoutExpired as e:
            output = (e.stdout or "") if isinstance(e.stdout, str) else ""
            observation = {
                "output": output,
                "returncode": -1,
                "exception_info": f"command timed out after {timeout}s",
            }
        except Exception as e:  # surfaced to the model as an observation, like their adapter
            observation = {"output": "", "returncode": -1,
                           "exception_info": f"error while executing command: {e}"}
        self._check_finished(observation)
        return observation

    @staticmethod
    def _check_finished(observation):
        """Mirror of mini-swe-agent's LocalEnvironment submit detection (their
        Harbor adapter copies the same semantics): first output line equal to
        the sentinel with exit 0 terminates the episode."""
        from minisweagent.exceptions import Submitted

        lines = observation.get("output", "").lstrip().splitlines(keepends=True)
        if lines and lines[0].strip() == SUBMIT and observation.get("returncode") == 0:
            submission = "".join(lines[1:])
            raise Submitted({"role": "exit", "content": submission,
                             "extra": {"exit_status": "Submitted", "submission": submission}})

    def get_template_vars(self, **kwargs):
        return {"cwd": self.config.cwd, "timeout": self.config.timeout, **kwargs}

    def serialize(self):
        return {"info": {"config": {"environment": {"cwd": self.config.cwd,
                                                    "timeout": self.config.timeout}}}}


def make_tracing_agent(turn_log_path):
    """DefaultAgent subclass appending one JSONL line per step — their
    TracingAgent shape, kept thin: the trajectory JSON is the full record."""
    from minisweagent.agents.default import DefaultAgent

    class TracingAgent(DefaultAgent):
        turn_log = None

        def step(self):
            t0 = time.time()
            error = None
            try:
                return super().step()
            except Exception as e:
                error = e
                raise
            finally:
                try:
                    record = {"turn": getattr(self, "n_calls", None),
                              "elapsed_sec": round(time.time() - t0, 2),
                              "cost_cumulative": getattr(self, "cost", None),
                              "error": None if error is None else f"{type(error).__name__}: {error}"}
                    with open(turn_log_path, "a") as f:
                        f.write(json.dumps(record, default=str) + "\n")
                except Exception:
                    pass

    return TracingAgent


# ---------------------------------------------------------------- subcommands

def cmd_setup(args):
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "bin").mkdir(exist_ok=True)
    env = {"venvs": {}, "redis": None}

    if not args.skip_redis:
        if REDIS_BIN.exists():
            log(f"redis: already built at {REDIS_BIN}")
        else:
            src = OUT / "upstream" / f"redis-{REDIS_VERSION}"
            tar = OUT / "upstream" / f"redis-{REDIS_VERSION}.tar.gz"
            tar.parent.mkdir(parents=True, exist_ok=True)
            if not tar.exists():
                log(f"redis: downloading {REDIS_VERSION}")
                subprocess.run(["curl", "-fsSL", "-o", str(tar),
                                f"https://github.com/redis/redis/archive/refs/tags/{REDIS_VERSION}.tar.gz"],
                               check=True)
            if not src.exists():
                subprocess.run(["tar", "xzf", str(tar), "-C", str(src.parent)], check=True)
            log("redis: building (a few minutes)")
            subprocess.run(["make", "-j", "4", "MALLOC=libc"], cwd=src, check=True,
                           stdout=subprocess.DEVNULL)
            shutil.copy2(src / "src" / "redis-server", REDIS_BIN)
        v = subprocess.run([str(REDIS_BIN), "--version"], capture_output=True, text=True)
        log(f"redis: {v.stdout.strip() or v.stderr.strip()}")
        env["redis"] = str(REDIS_BIN)

    tids = args.tasks.split(",") if args.tasks else list(TASKS)
    for tid in tids:
        t = load_task(tid)
        venv = OUT / "venvs" / tid
        if not venv.exists():
            log(f"{tid}: creating venv (python 3.12, seeded)")
            subprocess.run(["uv", "venv", "--seed", "--python", "3.12", str(venv)], check=True)
        pkgs = (t["deps"]["runtime"] + t["deps"]["build"]
                + ["pytest", "pytest-timeout"])
        log(f"{tid}: installing {len(pkgs)} packages")
        subprocess.run(["uv", "pip", "install", "-q", "-p", str(venv / "bin" / "python"), *pkgs],
                       check=True)
        base = subprocess.run([str(venv / "bin" / "python"), "-c",
                               "import sys;print(sys.base_prefix)"],
                              capture_output=True, text=True, check=True).stdout.strip()
        python_root = str(Path(base).parent.parent) if ".local/share/uv/python" in base else base
        env["venvs"][tid] = {"path": str(venv), "base_prefix": base,
                             "python_root": python_root}

        # The probe runs INSIDE the bwrap layout, so it validates the binds,
        # the venv, the deps and the offline boundary in one shot. Deps are
        # checked by installed distribution name (import names diverge from
        # package names — python-can imports as `can`); pytest is imported for
        # real. The editable-build proof happens in verify against setup.sh.
        e = BwrapEnvironment(app=OUT, venv=venv, base_prefix=base, python_root=python_root,
                             needs_redis=t["test"]["needs_redis"], timeout=120)
        dists = ", ".join(f"'{d.split('<')[0].split('>=')[0].strip()}'"
                          for d in t["deps"]["runtime"])
        probe_code = ("import pytest\n"
                      "from importlib.metadata import version\n"
                      f"missing=[d for d in ({dists}) if not version(d)]\n"
                      "assert not missing, 'missing: %r' % missing\n")
        probe = e.execute({"command": "python - <<'PYEOF'\n" + probe_code + "PYEOF"},
                          timeout=120)
        if probe["returncode"] != 0:
            log(f"{tid}: DEP PROBE FAILED inside sandbox:\n{probe['output'][-2000:]}")
            sys.exit(1)
        log(f"{tid}: pytest + {len(t['deps']['runtime'])} runtime deps present inside sandbox")
        if t["test"]["needs_redis"] and REDIS_BIN.exists():
            r = e.execute({"command": "/tmp/redis-server --version"}, timeout=15)
            log(f"{tid}: redis inside sandbox: {r['output'].strip().splitlines()[0][:80]}")

    # The offline gate: watched FAILING before it is trusted (ARCH §5). A curl
    # that exits 0 with a code other than 000 means the sandbox has network.
    e = BwrapEnvironment(app=OUT, venv=OUT / "venvs" / tids[0],
                         base_prefix=env["venvs"][tids[0]]["base_prefix"],
                         python_root=env["venvs"][tids[0]].get("python_root"))
    curl = e.execute({"command": "curl -m 4 -s https://pypi.org -o /dev/null -w '%{http_code}'; "
                                 "echo rc=$?"}, timeout=30)
    if "rc=0" in curl["output"] and "000" not in curl["output"]:
        log(f"OFFLINE GATE FAILED — network reached inside sandbox: {curl['output'][:200]}")
        sys.exit(1)
    log(f"offline gate watched failing (curl output: {curl['output'].strip()[-40:]})")

    (OUT / "env.json").write_text(json.dumps(env, indent=1) + "\n")
    log(f"setup: ok — env.json written")


def cmd_probe(args):
    import urllib.request

    with urllib.request.urlopen(f"{args.url.rstrip('/')}/models", timeout=10) as r:
        models = json.load(r)
    names = [m["id"] for m in models.get("data", [])]
    log(f"probe: /v1/models -> {names}")
    if not names:
        sys.exit(1)

    body = json.dumps({
        "model": names[0], "max_tokens": 8,
        "messages": [{"role": "user", "content": "ping"}],
        "chat_template_kwargs": {"enable_thinking": False},
    }).encode()
    req = urllib.request.Request(f"{args.url.rstrip('/')}/chat/completions", data=body,
                                 headers={"Content-Type": "application/json",
                                          "Authorization": "Bearer probe"})
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            resp = json.load(r)
        log(f"probe: chat 200 in {time.time()-t0:.1f}s, finish={resp['choices'][0].get('finish_reason')}")
    except Exception as e:
        log(f"probe: chat request failed: {e}")
        sys.exit(1)

    if args.arm_log:
        import re

        last = ""
        with open(args.arm_log, errors="replace") as f:
            for line in f:
                if "format_prompt: conversation rendered" in line:
                    last = re.sub(r"\x1b\[[0-9;]*m", "", line)
        if not last:
            log("probe: no conversation-render line in arm log — conversation path not taken")
            sys.exit(1)
        if "enable_thinking=false" not in last:
            log(f"probe: last render line does not show thinking off:\n{last.strip()[-300:]}")
            sys.exit(1)
        log("probe: arm log shows the conversation path with thinking off")


def cmd_run(args):
    import yaml
    from minisweagent.models.litellm_model import LitellmModel

    if not os.environ.get("OPENAI_API_BASE") and not os.environ.get("OPENAI_BASE_URL"):
        log("run: OPENAI_API_BASE is not set — point it at the tap")
        sys.exit(2)
    t = load_task(args.task)
    cfg = yaml.safe_load(UPSTREAM_CONFIG.read_text())
    run_id = args.run_id or f"{args.task}-{args.model}-{time.strftime('%Y%m%d-%H%M%S')}"
    run_dir = OUT / "runs" / run_id
    app = run_dir / "app"
    app.mkdir(parents=True, exist_ok=True)
    shutil.copy2(OUT / "tasks" / args.task / "instruction.md", app / "instruction.md")
    envd = env_json()["venvs"][args.task]

    meta = {"run_id": run_id, "task": args.task, "model": args.model,
            "url": os.environ.get("OPENAI_API_BASE") or os.environ.get("OPENAI_BASE_URL"),
            "wall_min": args.wall_min, "started_at": time.time(), "exit_status": "never_ran"}
    (run_dir / "meta.json").write_text(json.dumps(meta, indent=1) + "\n")

    log(f"run {run_id}: wall cap {args.wall_min}m, step_limit {cfg['agent'].get('step_limit')}")
    timer = threading.Timer(args.wall_min * 60, lambda: os._exit(124))
    timer.daemon = True
    timer.start()

    model = LitellmModel(
        **cfg.get("model", {}),
        model_name=f"openai/{args.model}",
        cost_tracking="ignore_errors",
        model_kwargs={"max_tokens": args.max_tokens, "timeout": 1800,
                      "chat_template_kwargs": {"enable_thinking": False}},
    )
    env = BwrapEnvironment(app=app, venv=Path(envd["path"]), base_prefix=Path(envd["base_prefix"]),
                           python_root=envd.get("python_root"),
                           needs_redis=t["test"]["needs_redis"])
    agent = make_tracing_agent(run_dir / "turns.jsonl")(
        model=model, env=env, output_path=str(run_dir / "trajectory.json"),
        **cfg["agent"])

    instruction = (OUT / "tasks" / args.task / "instruction.md").read_text()
    t0 = time.time()
    try:
        result = agent.run(instruction)
        if isinstance(result, dict):
            meta["exit_status"] = str(result.get("exit_status", "unknown"))
            meta["submission"] = str(result.get("submission", ""))[:2000]
        else:
            meta["exit_status"] = str(result)
    except Exception as e:
        meta["exit_status"] = f"error:{type(e).__name__}"
        meta["error"] = str(e)[:2000]
        log(f"run: agent raised {type(e).__name__}: {str(e)[:300]}")
    finally:
        timer.cancel()
        meta["turns"] = getattr(agent, "n_calls", None)
        meta["cost"] = getattr(agent, "cost", None)
        meta["wall_ms"] = int((time.time() - t0) * 1000)
        (run_dir / "meta.json").write_text(json.dumps(meta, indent=1) + "\n")
    log(f"run: exit_status={meta['exit_status']} turns={meta.get('turns')} "
        f"wall={meta['wall_ms']/1000:.0f}s")
    sys.exit(0)


def cmd_score(args):
    run_dir = OUT / "runs" / args.run_id
    meta = json.loads((run_dir / "meta.json").read_text())
    tid = meta["task"]
    t = load_task(tid)
    envd = env_json()["venvs"][tid]
    app = run_dir / "app"
    if not (app / "setup.sh").exists():
        log("score: no setup.sh in the workdir — verifying anyway so the pytest "
            "failure is the record, and the run reads as failed, not never_ran")
    # A run killed by the wall timer dies by os._exit(124): its meta.json still
    # says never_ran. The caller's observed rc is the truth — map it here (§6).
    if args.run_rc == 124 and meta.get("exit_status") == "never_ran":
        meta["exit_status"] = "wall_cap"
        (run_dir / "meta.json").write_text(json.dumps(meta, indent=1) + "\n")
    verify = run_dir / "verify"
    if verify.exists():
        shutil.rmtree(verify)
    log("score: copying workdir to a fresh verify tree")
    shutil.copytree(app, verify)

    target = t["test"]["target"]
    pytest_target = "/tests/" + target[len("tests/"):] if target.startswith("tests/") else target
    inner = (
        "bash ./setup.sh >/logs/setup.log 2>&1; echo setup_rc=$?; "
        f"pytest {pytest_target} --junitxml /logs/junit.xml -v "
        f"--timeout={t['test']['pytest_timeout']} -rA; exit $?"
    )
    e = BwrapEnvironment(app=verify, venv=Path(envd["path"]), base_prefix=Path(envd["base_prefix"]),
                         python_root=envd.get("python_root"),
                         needs_redis=t["test"]["needs_redis"], timeout=1800,
                         extra_binds=[(run_dir, "/logs", True),
                                      (OUT / "tasks" / tid / "tests", "/tests", False)])
    log(f"score: verify (setup.sh + pytest {pytest_target})")
    t0 = time.time()
    proc = subprocess.run(e.bwrap_argv("exec >>/logs/verify.log 2>&1; " + inner),
                          env=scrubbed_env(Path(envd["path"])), capture_output=True,
                          text=True, timeout=3600)
    wall = time.time() - t0

    counters, verify_incomplete = None, False
    junit = run_dir / "junit.xml"
    if junit.exists():
        sys.path.insert(0, str(LANE))
        from score_junit import score_junit

        try:
            counters = score_junit(str(junit))
        except Exception as ex:
            verify_incomplete = True
            log(f"score: junit unreadable ({ex})")
    else:
        verify_incomplete = True
        log("score: no junit report produced")

    setup_rc = None
    vlog = run_dir / "verify.log"
    if vlog.exists():
        for line in vlog.read_text(errors="replace").splitlines():
            if line.startswith("setup_rc="):
                setup_rc = int(line.split("=")[1])
    reward = (proc.returncode == 0) and not verify_incomplete

    row = {
        "schema": "e2eswe-slice-row/v1",
        "run_id": meta["run_id"], "task": tid, "model": meta["model"],
        "exit_status": meta["exit_status"],
        "setup_rc": setup_rc,
        "verify_exit": proc.returncode,
        "reward": 1 if reward else 0,
        "verify_incomplete": verify_incomplete,
        **(counters or {}),
        "turns": meta.get("turns"),
        "wall_ms": meta.get("wall_ms"),
        "verify_wall_ms": int(wall * 1000),
        "notes": "",
    }
    _append_row(row)
    log(f"score: reward={row['reward']} fraction={row.get('test_fraction')} "
        f"passed={row.get('tests_passed')}/{row.get('tests_total')} "
        f"setup_rc={setup_rc} verify_wall={wall:.0f}s")
    return 0


def _append_row(row):
    rows = []
    if RESULTS.exists():
        for line in RESULTS.read_text().splitlines():
            try:
                r = json.loads(line)
                if r.get("run_id") != row["run_id"]:
                    rows.append(r)
            except json.JSONDecodeError:
                pass
    rows.append(row)
    tmp = RESULTS.with_suffix(".tmp")
    tmp.write_text("".join(json.dumps(r) + "\n" for r in rows))
    tmp.replace(RESULTS)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("setup")
    s.add_argument("--tasks", default="")
    s.add_argument("--skip-redis", action="store_true")
    p = sub.add_parser("probe")
    p.add_argument("--url", required=True)
    p.add_argument("--arm-log", default="")
    r = sub.add_parser("run")
    r.add_argument("--task", required=True, choices=TASKS)
    r.add_argument("--model", required=True)
    r.add_argument("--wall-min", type=int, default=80)
    r.add_argument("--max-tokens", type=int, default=32768)
    r.add_argument("--run-id", default="")
    sc = sub.add_parser("score")
    sc.add_argument("--run-id", required=True)
    sc.add_argument("--run-rc", type=int, default=0,
                    help="exit code the `run` process observed (124 = wall cap)")
    args = ap.parse_args()
    {"setup": cmd_setup, "probe": cmd_probe, "run": cmd_run, "score": cmd_score}[args.cmd](args)


if __name__ == "__main__":
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    main()
