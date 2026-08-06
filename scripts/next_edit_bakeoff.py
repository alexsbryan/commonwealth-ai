#!/usr/bin/env python3
"""Next-edit build-vs-adopt bakeoff driver — Phase 0.

Spec: `sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md` §7 (Phase 0), §8 item 2.

For each arm in the manifest this brings up the candidate on llama-server,
puts `examples/next_edit_score` in front of it (which runs *the daemon's
own pipeline*, so the verdicts are the daemon's), runs the two
pre-registered banks unmodified, tears the arm down, and records the
result. Then it prints one comparison table.

Why a driver rather than a shell session: a bakeoff decides whether to
spend weeks of training, so every number it produces has to be
re-derivable months later from a file that names the model, the quant,
the wire format, and the server flags that produced it. A number typed
into a terminal once is not evidence.

Four verdicts, never two (ARCH §18.1): `pass`, `fail`, `could-not-judge`
(the arm ran but something upstream made the result unattributable), and
`never-ran` (the arm never started — a missing weight file is not a
model that scored zero).

    python3 scripts/next_edit_bakeoff.py --manifest sovereign/bench/next-edit-bakeoff/arms.toml
    python3 scripts/next_edit_bakeoff.py --only sweep-1.5b --keep-going
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SCORER = REPO / "target" / "debug" / "examples" / "next_edit_score"


def log(msg: str) -> None:
    print(f"[bakeoff] {msg}", flush=True)


def wait_http(url: str, timeout_s: float, what: str) -> bool:
    """Poll until `url` answers or the budget runs out."""
    deadline = time.monotonic() + timeout_s
    last = ""
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=3) as r:
                if r.status < 500:
                    return True
        except urllib.error.HTTPError as e:  # 4xx means it IS listening
            if e.code < 500:
                return True
            last = str(e)
        except Exception as e:
            last = str(e)
        time.sleep(1.0)
    log(f"  timed out waiting for {what} at {url} ({last})")
    return False


class Proc:
    """A child process that is always reaped, even on exception."""

    def __init__(self, name: str, argv: list[str], logfile: Path):
        self.name, self.argv, self.logfile = name, argv, logfile
        self.p: subprocess.Popen | None = None

    def __enter__(self):
        self.logfile.parent.mkdir(parents=True, exist_ok=True)
        self.fh = self.logfile.open("w")
        self.p = subprocess.Popen(
            self.argv, stdout=self.fh, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        return self

    def __exit__(self, *exc):
        if self.p and self.p.poll() is None:
            try:
                os.killpg(os.getpgid(self.p.pid), signal.SIGTERM)
                self.p.wait(timeout=20)
            except Exception:
                try:
                    os.killpg(os.getpgid(self.p.pid), signal.SIGKILL)
                except Exception:
                    pass
        self.fh.close()

    def died(self) -> bool:
        return self.p is not None and self.p.poll() is not None


def run_bank(script: str, endpoint: str, out_json: Path, timeout_s: float) -> dict:
    """Run one pre-registered bank against `endpoint`, unmodified."""
    out_json.parent.mkdir(parents=True, exist_ok=True)
    cmd = [sys.executable, str(REPO / "scripts" / script),
           "--endpoint", endpoint, "--json", str(out_json)]
    t0 = time.monotonic()
    r = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True, timeout=timeout_s)
    return {
        "script": script,
        "exit": r.returncode,
        "stdout": r.stdout.strip(),
        "stderr": r.stderr.strip()[-2000:],
        "wall_s": round(time.monotonic() - t0, 1),
        "json": str(out_json) if out_json.exists() else None,
    }


# Drops that mean "the model never got a fair hearing in our dialect"
# rather than "the model was wrong". `invalid` is our parser refusing the
# reply's shape; `truncated` is a decode that never terminated, which a
# wrong or missing stop string produces. Both are protocol-boundary
# failures. Content-level drops (`noop`, `inconsistent`, `already_applied`)
# are the model being judged on what it said, and are NOT counted here.
PROTOCOL_DROPS = ("invalid", "truncated")


def format_fidelity(gen_json: Path) -> dict | None:
    """Did our prompt dialect actually reach this model?

    A `positive` case that never produced a fire and died at the protocol
    boundary is evidence about the harness, not the checkpoint. When that
    dominates, the arm is `could-not-judge`.
    """
    if not gen_json.exists():
        return None
    try:
        rows = json.loads(gen_json.read_text())
    except Exception:
        return None
    pos = [c for c in rows if c.get("kind") == "positive"]
    if not pos:
        return None
    fires = sum(1 for c in pos if c.get("fired"))
    bad = [c.get("dropped") for c in pos if c.get("dropped") in PROTOCOL_DROPS]
    detail = ", ".join(f"{n}×{k}" for k, n in
                       sorted(collections.Counter(bad).items(), key=lambda kv: -kv[1]))
    # Threshold: more than half of consults dying at the protocol
    # boundary is not a bad model, it is a bad prompt. Deliberately
    # generous — a genuinely poor model still PARSES most of the time.
    under = len(bad) > len(pos) / 2 and fires == 0
    return {
        "consults": len(pos),
        "fires": fires,
        "protocol_fail": len(bad),
        "detail": detail or "none",
        "verdict": "under-served" if under else "served",
    }


def run_arm(arm: dict, args, outdir: Path) -> dict:
    aid = arm["id"]
    rec: dict = {
        "id": aid,
        "verdict": "never-ran",
        "model": arm.get("model"),
        "format": arm.get("format"),
        "quant": arm.get("quant"),
        "params": arm.get("params"),
        "license": arm.get("license"),
        "source": arm.get("source"),
    }

    model = Path(os.path.expanduser(arm["model"]))
    if not model.exists():
        # Absence is reported, never defaulted (ARCH §18.3). An arm with
        # no weights is `never-ran`; it is emphatically not a zero.
        rec["note"] = f"weight file absent: {model}"
        log(f"{aid}: SKIP — {rec['note']}")
        return rec
    rec["model_bytes"] = model.stat().st_size

    ls_port, sc_port = args.llama_port, args.scorer_port
    ls_log = outdir / f"{aid}.llama-server.log"
    sc_log = outdir / f"{aid}.scorer.log"

    llama = [
        args.llama_server, "-m", str(model),
        "--port", str(ls_port), "--host", "127.0.0.1",
        "--ctx-size", str(arm.get("ctx", args.ctx)),
        "-ngl", str(arm.get("ngl", args.ngl)),
        "--no-webui",
    ] + list(arm.get("extra_args", []))
    rec["llama_argv"] = llama

    with Proc("llama-server", llama, ls_log):
        if not wait_http(f"http://127.0.0.1:{ls_port}/health", args.load_timeout,
                         f"{aid} llama-server"):
            rec["verdict"] = "could-not-judge"
            rec["note"] = f"llama-server never became healthy; see {ls_log}"
            log(f"{aid}: COULD-NOT-JUDGE — server never came up")
            return rec
        log(f"{aid}: llama-server up")

        scorer = [
            str(SCORER), "--upstream", f"http://127.0.0.1:{ls_port}",
            "--format", arm["format"], "--model-id", aid,
            "--port", str(sc_port),
            "--concurrency", str(arm.get("concurrency", args.concurrency)),
            "--timeout-ms", str(args.consult_timeout_ms),
        ]
        rec["scorer_argv"] = scorer
        with Proc("scorer", scorer, sc_log):
            endpoint = f"http://127.0.0.1:{sc_port}"
            time.sleep(1.5)
            if not wait_http(f"{endpoint}/v1/edit_predictions", 20, f"{aid} scorer"):
                rec["verdict"] = "could-not-judge"
                rec["note"] = f"scorer never listened; see {sc_log}"
                return rec

            banks = {}
            # The rule bank is model-independent by construction. It runs
            # per arm anyway as a CONTROL: if it ever moves off 120/120,
            # the harness changed under us and the model numbers from the
            # same run are not attributable.
            if not args.skip_rule:
                banks["rule"] = run_bank(
                    "next_edit_eval.py", endpoint,
                    outdir / f"{aid}.rule.json", args.bank_timeout)
            banks["gen"] = run_bank(
                "next_edit_gen_eval.py", endpoint,
                outdir / f"{aid}.gen.json", args.bank_timeout)
            rec["banks"] = banks

    ctl = banks.get("rule")
    fid = format_fidelity(outdir / f"{aid}.gen.json")
    rec["format_fidelity"] = fid
    if ctl and ctl["exit"] != 0:
        rec["verdict"] = "could-not-judge"
        rec["note"] = ("rule-lane control did not pass on this arm — the model "
                       "numbers from this run are not attributable")
    elif fid and fid["verdict"] == "under-served":
        # A model we mis-served is UNMEASURED, not beaten. Recording it
        # as a loss is how a wrong prompt becomes a permanent, confident
        # verdict about somebody else's model (BAKEOFF §9). Caught live
        # 2026-08-05: zeta-2 scored 0/30 on a stale dialect and 27/30
        # once the markers were corrected.
        rec["verdict"] = "could-not-judge"
        rec["note"] = (
            f"format fidelity: {fid['protocol_fail']}/{fid['consults']} consults died at the "
            f"protocol boundary ({fid['detail']}) — this arm was mis-served by our "
            f"'{arm['format']}' dialect, so its score is not a verdict on the model"
        )
    else:
        rec["verdict"] = "pass" if banks["gen"]["exit"] == 0 else "fail"
    log(f"{aid}: {rec['verdict'].upper()}")
    for line in (banks["gen"]["stdout"] or "").splitlines()[-3:]:
        log(f"    {line}")
    return rec


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest",
                    default="sovereign/bench/next-edit-bakeoff/arms.toml")
    ap.add_argument("--out", default="sovereign/bench/next-edit-bakeoff/runs")
    ap.add_argument("--only", action="append", default=None,
                    help="run only these arm ids (repeatable)")
    ap.add_argument("--llama-server", default=shutil.which("llama-server") or "llama-server")
    ap.add_argument("--llama-port", type=int, default=8089)
    ap.add_argument("--scorer-port", type=int, default=9799)
    ap.add_argument("--ctx", type=int, default=8192)
    ap.add_argument("--ngl", type=int, default=99)
    ap.add_argument("--concurrency", type=int, default=1)
    ap.add_argument("--consult-timeout-ms", type=int, default=15000)
    ap.add_argument("--load-timeout", type=float, default=300.0)
    ap.add_argument("--bank-timeout", type=float, default=3600.0)
    ap.add_argument("--skip-rule", action="store_true",
                    help="skip the per-arm rule-lane control (not recommended)")
    ap.add_argument("--keep-going", action="store_true")
    ap.add_argument("--tag", default=None, help="run directory name")
    args = ap.parse_args()

    if not SCORER.exists():
        sys.exit(f"scorer not built: {SCORER}\n"
                 "  cargo build -p commonwealth-api --example next_edit_score")

    manifest = Path(args.manifest)
    if not manifest.is_absolute():
        manifest = REPO / manifest
    with manifest.open("rb") as fh:
        arms = tomllib.load(fh).get("arm", [])
    if args.only:
        arms = [a for a in arms if a["id"] in set(args.only)]
    if not arms:
        sys.exit("no arms selected")

    tag = args.tag or time.strftime("%Y-%m-%dT%H%M%S")
    outdir = Path(args.out)
    if not outdir.is_absolute():
        outdir = REPO / outdir
    outdir = outdir / tag
    outdir.mkdir(parents=True, exist_ok=True)
    log(f"{len(arms)} arm(s) → {outdir}")

    results = []
    for arm in arms:
        log(f"--- {arm['id']} ---")
        try:
            results.append(run_arm(arm, args, outdir))
        except Exception as e:  # noqa: BLE001 — one bad arm must not eat the run
            log(f"{arm['id']}: could-not-judge — driver error: {e}")
            results.append({"id": arm["id"], "verdict": "could-not-judge",
                            "note": f"driver error: {e}"})
            if not args.keep_going:
                break

    summary = {
        "tag": tag,
        "manifest": str(manifest),
        "llama_server": args.llama_server,
        "ctx": args.ctx,
        "arms": results,
    }
    (outdir / "summary.json").write_text(json.dumps(summary, indent=2))

    print()
    print("=" * 78)
    print(" next-edit bakeoff — Phase 0")
    print("=" * 78)
    print(f" {'arm':<20} {'params':>7} {'quant':>7} {'format':>16} {'verdict':>16}")
    for r in results:
        print(f" {r['id']:<20} {str(r.get('params') or '-'):>7} "
              f"{str(r.get('quant') or '-'):>7} {str(r.get('format') or '-'):>16} "
              f"{r['verdict']:>16}")
        if r.get("note"):
            print(f"   └─ {r['note']}")
    print()
    print(f" full results: {outdir}/summary.json")

    ran = [r for r in results if r["verdict"] in ("pass", "fail")]
    if not ran:
        print(" NO ARM PRODUCED A JUDGEABLE RESULT — this is not a verdict on any model.")
        sys.exit(4)


if __name__ == "__main__":
    main()
