#!/usr/bin/env python3
"""Run ONE arm of PRE-REG-custom-ontology-and-raptor-2026-09-17, once, and
record what it was.

    run_arm.py --arm full --bank P/bank.toml --corpus chaos-secret-agent \
               --index-dir ~/.svrnmesh/indexes/chaos-secret-agent \
               --recipe sovereign-recipes/.../recipe.toml --out target/ei7-runs

Writes `<out>/<arm>/run-<N>/eval.json` (the eval CLI's own run JSON) and
`<out>/<arm>/run-<N>/manifest.json` (what this run WAS, so a later comparison
can refuse arms that are not comparable).

The arms are data (`arms.toml`, beside this file); this script owns only the
invocation the pre-reg fixes for every arm — `eval run --bank <b> --synth
--isolate --format json` — plus `--output`, which is the copy of the run JSON
that is kept (stdout is discarded; `--output` and stdout carry the same
document).

Exit codes. 0 = the manifest was written; the run's ADMISSIBILITY is in the
manifest, not in this code, because `never-ran` is a recorded outcome and not a
crash (checks.py issues the four verdicts). 2 = a premise refusal, nothing was
written and nothing was run.

The manifest only ever asserts `never-ran`. It cannot know `passed`, `failed`
or `could-not-judge` — those are the comparison's to make from eval.json — so
`verdict` is `null` rather than a verdict this run did not measure
(ARCH §18.3: absence is reported, never defaulted).
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from pathlib import Path

HARNESS = Path(__file__).resolve().parent
REPO = HARNESS.parents[2]
ARMS_TOML = HARNESS / "arms.toml"

# The fixed half of the command. The pre-reg ("Arms") fixes this for every arm;
# an arm's own `flags` are appended to it.
EVAL_BASE_FLAGS = ["--synth", "--isolate", "--format", "json"]

# The daemon endpoint, resolved exactly the way the run itself resolves it:
# `SOVEREIGN_DAEMON_URL` then `SVRNMESH_DAEMON_URL`, a set-but-blank value
# treated as unset, trailing slashes trimmed
# (sovereign-contracts/src/setup_config.rs:1695-1703). A rented pod on :9841 is
# therefore recorded as that host, not as localhost.
DAEMON_URL_KEYS = ("SOVEREIGN_DAEMON_URL", "SVRNMESH_DAEMON_URL")
DAEMON_DEFAULT = "http://localhost:9741"

# Synthesis runs on the daemon's primary slot: `run` passes the id that alias
# resolves to as `--chat-model`, the same id the manifest records;
# the in-loop judge is a fast-slot call (eval_cmd/runner.rs:220, :1575). Both
# are read back from the daemon rather than assumed, because the aliases move.
SYNTH_ALIAS = "primary"
JUDGE_ALIAS = "fast"
_ALIAS_RE = re.compile(r"^alias\s*(?:→|->)\s*(.+)$")

# A refused question scores as a miss and widens the noise band, so a run that
# met one is not a run (spike 3 lost a section to this on a shared daemon,
# 2026-09-19). Word-bounded on BOTH tokens: `retrieved 5031 chunks` is not a
# 503, and `ghost busywork` is not `host busy` — a substring search counts both.
REFUSAL_RE = re.compile(r"\b503\b|\bhost busy\b", re.IGNORECASE)
REFUSAL_SAMPLE_MAX = 10


class Refused(Exception):
    """A premise this script cannot proceed under. Exit 2, write nothing."""


# ── the pieces the manifest is made of ───────────────────────────────────────

def load_arms(path=ARMS_TOML):
    with open(path, "rb") as fh:
        doc = tomllib.load(fh)
    arms = {}
    for arm in doc.get("arm", []):
        arms[arm["id"]] = arm
    return arms


def arm_env(arm, pool_scale):
    """The arm's own env, with `${pool_scale}` substituted.

    Refuses rather than guessing: the deep-pool multiplier is fixed at
    ratification together with the model, and an unfixed arm is never-ran, not
    a value someone picked here (pre-reg, "Deep pool has no knob today").
    """
    out = {}
    for key, value in (arm.get("env") or {}).items():
        if "${pool_scale}" in value:
            if pool_scale is None:
                raise Refused(
                    f"arm `{arm['id']}` sets {key} from --pool-scale and none was "
                    f"given; the multiplier is the operator's, not this script's"
                )
            value = value.replace("${pool_scale}", str(pool_scale))
        out[key] = value
    return out


def sha256_file(path):
    """sha256 of a file, or None when it is not there.

    None is the record of an absent file — never sha256(b"") , which is a real
    hash of a real empty document and would compare equal to one.
    """
    p = Path(path)
    if not p.is_file():
        return None
    h = hashlib.sha256()
    with p.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def listing_sha256(root):
    """sha256 over the sorted (name, size) listing of a directory tree.

    Names are RELATIVE to `root`: a lance dataset copied to a second host under
    a different absolute path is the same dataset, and a manifest that said
    otherwise would refuse a comparison for a reason with nothing to do with
    its content (the rule `scripts/needle_rig.py:296-320` records for the same
    reason). Returns None when the tree is not there.
    """
    p = Path(root)
    if not p.is_dir():
        return None
    rows = sorted(
        (f.relative_to(p).as_posix(), f.stat().st_size)
        for f in p.rglob("*") if f.is_file()
    )
    h = hashlib.sha256()
    for name, size in rows:
        h.update(f"{name}\x00{size}\n".encode())
    return h.hexdigest()


def daemon_base(env):
    for key in DAEMON_URL_KEYS:
        raw = env.get(key)
        if raw is None:
            continue
        trimmed = raw.strip().rstrip("/")
        if trimmed:
            return trimmed
    return DAEMON_DEFAULT


def daemon_models(base, timeout=10.0):
    """Synth and judge model ids as the daemon reports them.

    An unreachable daemon — or one that does not report the alias — records
    None, and the run is never-ran. It is not defaulted to the id this host
    happens to load: "did not answer" is not "answered: <model>".
    """
    url = f"{base}/v1/models"
    out = {"synth": None, "judge": None, "source": url, "error": None}
    try:
        with urllib.request.urlopen(url, timeout=timeout) as resp:
            doc = json.loads(resp.read().decode())
    except (urllib.error.URLError, OSError, ValueError, TimeoutError) as e:
        out["error"] = f"{type(e).__name__}: {e}"
        return out
    by_id = {m.get("id"): m for m in doc.get("data", []) if m.get("id")}

    def resolve(alias):
        entry = by_id.get(alias)
        if entry is None:
            return None
        m = _ALIAS_RE.match(str(entry.get("owned_by") or ""))
        target = m.group(1).strip() if m else entry["id"]
        return target or None

    out["synth"] = resolve(SYNTH_ALIAS)
    out["judge"] = resolve(JUDGE_ALIAS)
    if out["synth"] is None or out["judge"] is None:
        missing = [a for a, k in ((SYNTH_ALIAS, "synth"), (JUDGE_ALIAS, "judge"))
                   if out[k] is None]
        out["error"] = f"daemon reports no model for alias(es): {', '.join(missing)}"
    return out


def count_refusals(stderr_lines):
    hits = [ln.rstrip("\n") for ln in stderr_lines if REFUSAL_RE.search(ln)]
    return {"count": len(hits), "sample": hits[:REFUSAL_SAMPLE_MAX]}


def git_provenance(repo=REPO):
    def git(*args):
        return subprocess.run(["git", "-C", str(repo), *args],
                              capture_output=True, text=True).stdout
    return {
        "commit": git("rev-parse", "HEAD").strip() or None,
        "dirty": bool(git("status", "--porcelain").strip()),
    }


def build_manifest(*, arm_id, variant, corpus, index_dir, recipe, run_index,
                   env, argv, base, models, refusals, eval_exit, git=None):
    """The manifest. `identity` is exactly what a comparison may refuse on."""
    reasons = []
    if models["error"]:
        reasons.append(f"daemon at {base}: {models['error']}")
    if refusals["count"]:
        reasons.append(f"{refusals['count']} daemon refusal(s) in the run's stderr")
    if eval_exit not in (0, None):
        reasons.append(f"eval exited {eval_exit}")
    return {
        "schema": "ei7-arm-manifest/v1",
        "identity": {
            "arm": arm_id,
            "corpus": corpus,
            "host": base,
            "synth_model": models["synth"],
            "judge_model": models["judge"],
            "recipe_sha256": sha256_file(recipe),
            "ontology_sha256": sha256_file(Path(index_dir) / "atlas" / "ontology.json"),
            "chunks_listing_sha256": listing_sha256(Path(index_dir) / "chunks.lance"),
        },
        "variant": variant,
        "run": run_index,
        "env": env,
        "argv": argv,
        "eval_exit": eval_exit,
        "daemon_refusals": refusals,
        "models_source": models["source"],
        "verdict": "never-ran" if reasons else None,
        "never_ran_reasons": reasons,
        "provenance": {
            **(git if git is not None else git_provenance()),
            "started_at_unix": int(time.time()),
            "paths": {
                "index_dir": str(index_dir),
                "recipe": str(recipe),
            },
        },
    }


# ── the run ──────────────────────────────────────────────────────────────────

def eval_binary():
    for name in ("svrn", "sovereign"):
        found = shutil.which(name)
        if found:
            return found
    raise Refused("neither `svrn` nor `sovereign` is on PATH")


def run(args):
    arms = load_arms()
    arm = arms.get(args.arm)
    if arm is None:
        raise Refused(f"unknown arm `{args.arm}` — arms.toml declares: "
                      f"{', '.join(sorted(arms))}")
    for label, path in (("--bank", args.bank), ("--recipe", args.recipe)):
        if not Path(path).is_file():
            raise Refused(f"{label} {path} is not a file")
    if not Path(args.index_dir).is_dir():
        raise Refused(f"--index-dir {args.index_dir} is not a directory")

    env_overlay = arm_env(arm, args.pool_scale)
    binary = eval_binary()
    out_run = Path(args.out) / args.arm / f"run-{args.run}"
    eval_json = out_run / "eval.json"
    argv = [binary, "eval", "run", "--bank", str(args.bank), *EVAL_BASE_FLAGS,
            "--output", str(eval_json), *(arm.get("flags") or [])]

    env = {**os.environ, **env_overlay}
    base = daemon_base(env)

    if args.dry_run:
        json.dump({"argv": argv, "env": env_overlay, "host": base,
                   "out": str(out_run)}, sys.stdout, indent=2)
        sys.stdout.write("\n")
        return 0

    out_run.mkdir(parents=True, exist_ok=True)
    models = daemon_models(base)
    # The id the manifest records is the id the eval asks for. With no
    # `--chat-model` the CLI sends THIS host's configured id, which a daemon
    # elsewhere refuses (pod window 20260921T034847Z: 24/24 turns, IQ4_NL asked
    # of a Q6_K pod). Unresolved stays unpassed: that run is never-ran below.
    if models["synth"]:
        argv += ["--chat-model", models["synth"]]

    print(f"arm {args.arm} run {args.run} -> {out_run}", file=sys.stderr)
    print(f"  host {base}  synth {models['synth']}  judge {models['judge']}",
          file=sys.stderr)
    proc = subprocess.Popen(argv, env=env, cwd=str(REPO), text=True,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                            bufsize=1)
    stderr_lines = []
    for line in proc.stderr:            # teed, so a pod run is not silent
        sys.stderr.write(line)
        stderr_lines.append(line)
    eval_exit = proc.wait()

    manifest = build_manifest(
        arm_id=args.arm, variant=arm.get("variant"), corpus=args.corpus,
        index_dir=args.index_dir, recipe=args.recipe, run_index=args.run,
        env=env_overlay, argv=argv, base=base, models=models,
        refusals=count_refusals(stderr_lines), eval_exit=eval_exit)
    (out_run / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

    verdict = manifest["verdict"] or "recorded (the comparison judges it)"
    print(f"  verdict: {verdict}", file=sys.stderr)
    for reason in manifest["never_ran_reasons"]:
        print(f"    - {reason}", file=sys.stderr)
    return 0


# ── self-test ────────────────────────────────────────────────────────────────

def self_test():
    """The failing inputs this script is named for.

    Each case carries a PLANT: an input the obvious-but-wrong implementation
    gets wrong. The verdicts are the four the queue names; a case that could
    not be set up reads `could-not-judge`, never `passed`.
    """
    import tempfile

    results = []

    def case(name, fn):
        try:
            ok, detail = fn()
        except Exception as e:                     # noqa: BLE001 — a broken fixture
            results.append((name, "could-not-judge", f"{type(e).__name__}: {e}"))
            return
        results.append((name, "passed" if ok else "failed", detail))

    fixed_git = {"commit": "0" * 40, "dirty": False}
    fixed_models = {"synth": "S", "judge": "J", "source": "x", "error": None}
    no_refusals = {"count": 0, "sample": []}

    def index_dir(root, ontology, chunk_files):
        d = Path(root) / "idx"          # same basename on both sides
        (d / "atlas").mkdir(parents=True)
        if ontology is not None:
            (d / "atlas" / "ontology.json").write_text(ontology)
        for rel, body in chunk_files.items():
            f = d / "chunks.lance" / rel
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(body)
        return d

    def manifest_for(idx, recipe):
        return build_manifest(
            arm_id="full", variant="both", corpus="c", index_dir=idx,
            recipe=recipe, run_index=1, env={}, argv=[], base="http://h:9741",
            models=fixed_models, refusals=no_refusals, eval_exit=0, git=fixed_git)

    def ontology_hash_is_the_only_difference():
        # PLANT: the two trees differ by ONE byte inside atlas/ontology.json and
        # in nothing else. An implementation that hashed the index dir wholesale,
        # or that ignored the ontology, gets the differing-key set wrong.
        with tempfile.TemporaryDirectory() as ta, tempfile.TemporaryDirectory() as tb:
            recipe = Path(ta) / "recipe.toml"
            recipe.write_text("[recipe]\nid='r'\n")
            chunks = {"data/0.lance": "AAAA", "_versions/1.manifest": "BB"}
            a = manifest_for(index_dir(ta, '{"types":["coin"]}', chunks), recipe)
            b = manifest_for(index_dir(tb, '{"types":["hoard"]}', chunks), recipe)
            differing = {k for k in a["identity"]
                         if a["identity"][k] != b["identity"][k]}
            ok = a["identity"] != b["identity"] and differing == {"ontology_sha256"}
            return ok, f"differing identity keys: {sorted(differing) or 'none'}"

    def missing_ontology_is_null_not_empty_hash():
        # PLANT: no atlas/ontology.json at all. sha256(b"") is a real hash of a
        # real empty file and would compare EQUAL to an arm that has one.
        empty = hashlib.sha256(b"").hexdigest()
        with tempfile.TemporaryDirectory() as t:
            recipe = Path(t) / "recipe.toml"
            recipe.write_text("x")
            m = manifest_for(index_dir(t, None, {"data/0.lance": "A"}), recipe)
            got = m["identity"]["ontology_sha256"]
            return got is None and got != empty, f"ontology_sha256 = {got!r}"

    def chunks_listing_is_name_and_size_only():
        # PLANT: the same (name, size) listing under two different absolute
        # paths, written in a different order. A hash over absolute paths, or
        # one that followed directory order, splits an identical dataset — and
        # I3 ("identical chunks.lance") would then never hold.
        with tempfile.TemporaryDirectory() as ta, tempfile.TemporaryDirectory() as tb, \
                tempfile.TemporaryDirectory() as tc:
            a = index_dir(ta, "{}", {"data/0.lance": "AAAA", "_versions/1.m": "BB"})
            b = index_dir(tb, "{}", {"_versions/1.m": "CC", "data/0.lance": "DDDD"})
            same = listing_sha256(a / "chunks.lance") == listing_sha256(b / "chunks.lance")
            c = index_dir(tc, "{}", {"data/0.lance": "AAAAA", "_versions/1.m": "BB"})
            grew = listing_sha256(a / "chunks.lance") != listing_sha256(c / "chunks.lance")
            missing_is_null = listing_sha256(Path(ta) / "nope") is None
            return (same and grew and missing_is_null,
                    f"path-independent={same} size-sensitive={grew} absent-is-null={missing_is_null}")

    def refusals_are_word_bounded():
        # PLANT: two lines that a substring search counts and must not —
        # `5031` is not a 503, and `ghost busywork` contains "host busy".
        lines = ["HTTP 503 upstream\n", "warn: host busy, retrying\n",
                 "retrieved 5031 chunks\n", "ghost busywork queued\n",
                 "all good\n"]
        got = count_refusals(lines)
        return got["count"] == 2, f"count={got['count']} sample={got['sample']}"

    def refusal_or_unreachable_daemon_is_never_ran():
        # PLANT: a daemon on a closed port. The models must be null and the run
        # must read never-ran, not scored against a defaulted model id.
        models = daemon_models("http://127.0.0.1:1", timeout=1.0)
        with tempfile.TemporaryDirectory() as t:
            recipe = Path(t) / "r.toml"
            recipe.write_text("x")
            idx = index_dir(t, "{}", {"data/0.lance": "A"})
            dead = build_manifest(
                arm_id="bare", variant="both", corpus="c", index_dir=idx,
                recipe=recipe, run_index=1, env={}, argv=[], base="http://127.0.0.1:1",
                models=models, refusals=no_refusals, eval_exit=0, git=fixed_git)
            refused = build_manifest(
                arm_id="bare", variant="both", corpus="c", index_dir=idx,
                recipe=recipe, run_index=1, env={}, argv=[], base="http://h:9741",
                models=fixed_models, refusals={"count": 3, "sample": []},
                eval_exit=0, git=fixed_git)
            clean = manifest_for(idx, recipe)
        ok = (dead["verdict"] == "never-ran"
              and dead["identity"]["synth_model"] is None
              and dead["identity"]["judge_model"] is None
              and refused["verdict"] == "never-ran"
              and clean["verdict"] is None)
        return ok, (f"unreachable={dead['verdict']} refused={refused['verdict']} "
                    f"clean={clean['verdict']!r}")

    def arms_are_the_pre_regs_six():
        arms = load_arms()
        want = {"closed-book", "bare", "deep", "ablation", "full", "oracle"}
        bare3 = {"SOVEREIGN_ATLAS_GROUNDING": "0", "SOVEREIGN_ATOM_ENUM": "0",
                 "SOVEREIGN_ATOM_ENUM_OVERVIEW": "0"}
        ok = (set(arms) == want
              and arms["closed-book"]["flags"] == ["--closed-book"]
              and arms["bare"]["env"] == bare3
              and arms["deep"]["env"] == {**bare3,
                                          "SOVEREIGN_KQ_POOL_SCALE": "${pool_scale}"}
              and all(a["variant"] in ("ontology", "literary", "both")
                      for a in arms.values()))
        return ok, f"arms: {sorted(arms)}"

    def deep_without_pool_scale_refuses():
        # PLANT: the deep arm with no --pool-scale. A substitution of 1 here
        # would report a bare run as a deep-pool run.
        arms = load_arms()
        substituted = arm_env(arms["deep"], 4)
        try:
            arm_env(arms["deep"], None)
        except Refused as e:
            return (substituted["SOVEREIGN_KQ_POOL_SCALE"] == "4",
                    f"substituted=4 ok; absent refused: {e}")
        return False, "arm_env substituted a pool scale nobody chose"

    case("ontology-hash-is-only-difference", ontology_hash_is_the_only_difference)
    case("missing-ontology-is-null", missing_ontology_is_null_not_empty_hash)
    case("chunks-listing-name-and-size", chunks_listing_is_name_and_size_only)
    case("refusals-word-bounded", refusals_are_word_bounded)
    case("unreachable-or-refused-is-never-ran", refusal_or_unreachable_daemon_is_never_ran)
    case("arms-are-the-pre-regs-six", arms_are_the_pre_regs_six)
    case("deep-without-pool-scale-refuses", deep_without_pool_scale_refuses)

    for name, verdict, detail in results:
        print(f"  {name:<38} {verdict:<16} {detail}", file=sys.stderr)
    bad = [n for n, v, _ in results if v != "passed"]
    print(f"self-test: {len(results) - len(bad)}/{len(results)} passed",
          file=sys.stderr)
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--arm", help="an id from arms.toml")
    ap.add_argument("--bank", help="the eval bank TOML")
    ap.add_argument("--corpus", help="corpus id the arm is pointed at (recorded)")
    ap.add_argument("--index-dir", help="<data>/indexes/<corpus>; holds atlas/ and chunks.lance")
    ap.add_argument("--recipe", help="the recipe TOML this corpus was built from")
    ap.add_argument("--out", help="runs root; this run lands in <out>/<arm>/run-<N>")
    ap.add_argument("--run", type=int, default=1, help="run index (default 1)")
    ap.add_argument("--pool-scale", type=int, default=None,
                    help="SOVEREIGN_KQ_POOL_SCALE for the deep arm; the range is "
                         "the Rust decider's (runtime::prompts::kq_pool_scale)")
    ap.add_argument("--dry-run", action="store_true",
                    help="print the resolved argv, env and host; run nothing")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    for name in ("bank", "index_dir", "recipe", "out"):
        if getattr(args, name) is not None:
            setattr(args, name, os.path.expanduser(getattr(args, name)))
    missing = [f"--{n.replace('_', '-')}" for n in
               ("arm", "bank", "corpus", "index_dir", "recipe", "out")
               if getattr(args, n) is None]
    if missing:
        ap.error("missing required argument(s): " + ", ".join(missing))
    try:
        return run(args)
    except Refused as e:
        print(f"run_arm: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
