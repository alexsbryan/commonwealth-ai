#!/usr/bin/env python3
"""Prepare, run, and score the frozen paired production reader trial.

    python3 research/ontology-apps/reader_trial.py prepare
    python3 research/ontology-apps/reader_trial.py run
    python3 research/ontology-apps/reader_trial.py score

`prepare` stops before extraction. `run` is a separate parent-launched action.
"""
import argparse
import datetime
import json
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import time

from reader_trial_data import (
    APP, ARM_IDS, BASE_URL, CHAT_MODEL, DATA, EMBED_MODEL, PREREG, REPO, ROOT,
    arm_specs, cli_env, freeze_binary, freeze_tree, git_stamp, index_snapshot,
    load_prereg, render_recipe, sha256_file, source_ids, stage_uv_inputs,
    stage_ward_inputs, validate_fresh_targets, write_json,
)
from reader_trial_scoring import (
    chapter_count, copy_once, phase_cost_lines, read_checkpoint,
    read_status_report, runtime_paths, score_trials, token_snapshot,
)


def invoke(name, argv, log_path, *, env=None):
    """Run one CLI step and preserve both streams, exit code, and elapsed time."""
    log_path = pathlib.Path(log_path)
    if log_path.exists():
        raise FileExistsError(f"refusing to replace invocation log: {log_path}")
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started, started_at = time.monotonic(), datetime.datetime.now(datetime.timezone.utc).isoformat()
    child_env = os.environ.copy()
    if env:
        child_env.update(env)
    try:
        completed = subprocess.run(argv, cwd=REPO, env=child_env, text=True, capture_output=True)
        stdout, stderr, code, error = completed.stdout, completed.stderr, completed.returncode, None
    except OSError as exc:
        stdout, stderr, code, error = "", "", None, f"{type(exc).__name__}: {exc}"
    elapsed = round(time.monotonic() - started, 3)
    record = {
        "name": name, "argv": argv, "started_at": started_at,
        "elapsed_seconds": elapsed, "exit_code": code, "error": error, "log": str(log_path),
    }
    lines = [f"$ {shlex.join(argv)}", f"exit_code={code}", f"elapsed_seconds={elapsed}"]
    if error:
        lines.append(f"spawn_error={error}")
    log_path.write_text("\n".join([*lines, "--- stdout ---", stdout, "--- stderr ---", stderr]), encoding="utf-8")
    print(json.dumps({"step": name, "exit_code": code, "elapsed_seconds": elapsed, "log": str(log_path)}))
    return {**record, "stdout": stdout, "stderr": stderr}


def append_jsonl(path, value):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(value, sort_keys=True) + "\n")


def record_step(result):
    return {key: value for key, value in result.items() if key not in {"stdout", "stderr"}}


def prepare():
    prereg = load_prereg()
    validate_fresh_targets(ROOT)
    ROOT.mkdir()
    for name in ("arms", "runs", "scores", "bin", "logs/prepare"):
        (ROOT / name).mkdir(parents=True, exist_ok=True)
    state = {
        "trial_id": prereg["trial"]["id"], "root": str(ROOT), "status": "preparing",
        "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(), "arms": {}, "steps": [],
    }
    try:
        staged = {"uv": stage_uv_inputs(ROOT, prereg), "ward": stage_ward_inputs(ROOT, prereg)}
        write_json(ROOT / "inputs/source-selection.json", staged)
        shutil.copyfile(PREREG, ROOT / "reader-trial-prereg.toml")
        binaries = {
            "ingest": freeze_binary(REPO / "target/debug/svrn-ingest", ROOT / "bin/svrn-ingest", "ingest"),
            "bench": freeze_binary(REPO / "target/debug/sovereign-cli-bench", ROOT / "bin/sovereign-cli-bench", "bench"),
        }
        env, specs = cli_env(ROOT), arm_specs(prereg)
        recipe_dir = ROOT / "recipes"
        recipe_dir.mkdir()
        for arm in specs:
            corpus_id, app = arm["corpus_id"], arm["application"]
            source = ROOT / ("inputs/uv/raw/documents.jsonl" if app == "uv" else "inputs/ward/sample")
            path = recipe_dir / f"{corpus_id}.toml"
            path.write_text(render_recipe(app, corpus_id, source, bool(arm["document_reading"])), encoding="utf-8")
            arm["recipe_path"], arm["recipe_sha256"] = str(path), sha256_file(path)
            state["arms"][corpus_id] = {"status": "staged", "steps": [], "recipe_sha256": arm["recipe_sha256"]}
        write_json(ROOT / "stamps.json", {
            "trial_id": prereg["trial"]["id"], "git": git_stamp(), "binaries": binaries,
            "staged_inputs": staged,
            "recipes": {arm["corpus_id"]: arm["recipe_sha256"] for arm in specs},
        }, readonly=True)
        freeze_tree(ROOT / "inputs")
        freeze_tree(recipe_dir)
        (ROOT / "reader-trial-prereg.toml").chmod(0o444)

        ledger = ROOT / "logs/prepare/invocations.jsonl"
        listing = invoke("recipe-list", ["sovereign", "recipe", "list", "--offline"], ROOT / "logs/prepare/recipe-list.log", env=env)
        state["steps"].append(record_step(listing))
        append_jsonl(ledger, record_step(listing))
        if listing.get("exit_code") != 0:
            raise RuntimeError("recipe list preflight failed")
        registered = {
            line.split()[0] for line in listing["stdout"].splitlines() if line.strip()
            and not line.lstrip().startswith(("ID ", "─", "corpus ", "33 corpus", "(offline"))
        }
        collisions = sorted(set(ARM_IDS) & registered)
        if collisions:
            raise RuntimeError(f"corpus IDs already registered; no publish attempted: {collisions}")

        for arm in specs:
            corpus_id, app = arm["corpus_id"], arm["application"]
            arm_state = state["arms"][corpus_id]
            arm_dir = ROOT / "arms" / corpus_id
            arm_dir.mkdir()
            recipe = arm["recipe_path"]
            commands = [
                ("validate", ["sovereign", "recipe", "validate", recipe, "--offline"]),
                ("publish", ["sovereign", "recipe", "publish", recipe]),
                ("ingest", ["sovereign", "ingest", recipe, "--base-url", BASE_URL, "--embed-model", EMBED_MODEL, "--no-enrich"]),
            ]
            failed = False
            for name, argv in commands:
                result = invoke(name, argv, ROOT / f"logs/prepare/{corpus_id}-{name}.log", env=env)
                entry = record_step(result)
                arm_state["steps"].append(entry)
                append_jsonl(ledger, entry)
                if result.get("exit_code") != 0:
                    arm_state.update({"status": "failed", "failed_step": name, "exit_code": result.get("exit_code")})
                    failed = True
                    break
                if name == "ingest":
                    status = invoke("corpus-status", ["sovereign", "corpus", "status", corpus_id], ROOT / f"logs/prepare/{corpus_id}-corpus-status.log", env=env)
                    status_record = record_step(status)
                    arm_state["steps"].append(status_record)
                    append_jsonl(ledger, status_record)
                    arm_state["corpus_status_exit_code"] = status.get("exit_code")
                    arm_state["index"] = index_snapshot(corpus_id, app, source_ids(app, staged), ROOT / f"inputs/{app}")
            if failed:
                continue
            init = invoke(
                "enrich-init",
                ["sovereign", "enrich", "init", corpus_id, "--from-corpus", corpus_id,
                 "--chat-model", CHAT_MODEL, "--embed-model", EMBED_MODEL,
                 "--min-section-body-words", "0", "--max-chapter-words", "2000"],
                ROOT / f"logs/prepare/{corpus_id}-enrich-init.log", env=env,
            )
            init_record = record_step(init)
            arm_state["steps"].append(init_record)
            append_jsonl(ledger, init_record)
            if init.get("exit_code") != 0:
                arm_state.update({"status": "failed", "failed_step": "enrich-init", "exit_code": init.get("exit_code")})
                continue
            paths, errors = runtime_paths(corpus_id), []
            for name in ("config", "chapters"):
                if not paths[name].is_file():
                    errors.append(f"missing {name}: {paths[name]}")
                else:
                    copy_once(paths[name], arm_dir / f"{name}.json")
            arm_state["chapter_count"] = chapter_count(paths["chapters"])
            arm_state["models"] = {"chat_model": CHAT_MODEL, "embed_model": EMBED_MODEL}
            if arm_state["chapter_count"] is None:
                errors.append("chapter manifest has no chapters array")
            arm_state["status"] = "ready" if not errors else "could_not_judge"
            if errors:
                arm_state["artifact_errors"] = errors
            write_json(arm_dir / "prepare.json", arm_state, readonly=True)
        state["status"] = "ready" if all(state["arms"].get(cid, {}).get("status") == "ready" for cid in ARM_IDS) else "partial"
        state["staged_input_counts"] = {
            app: {key: value for key, value in staged[app].items() if key.endswith("count") or key == "folder_counts"}
            for app in ("uv", "ward")
        }
    except Exception as exc:
        state.update({"status": "failed", "error": f"{type(exc).__name__}: {exc}"})
    state["completed_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    write_json(ROOT / "preparation.json", state, readonly=True)
    print(json.dumps({"preparation": state["status"], "root": str(ROOT), "arms": state["arms"]}, indent=2))
    return 0 if state["status"] == "ready" else 1


def archive_pre_run(corpus_id, run_dir):
    paths = runtime_paths(corpus_id)
    return {
        name: copy_once(paths[name], run_dir / f"artifacts/{name}.json")
        for name in ("config", "chapters")
    }


def run_arm(arm, prereg, prep):
    corpus_id, app = arm["corpus_id"], arm["application"]
    output = ROOT / "runs" / corpus_id
    if output.exists():
        raise FileExistsError(f"run already archived; refusing implicit retry: {output}")
    output.mkdir()
    state = {
        "corpus_id": corpus_id, "application": app, "document_reading": bool(arm["document_reading"]),
        "status": "running", "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "steps": [], "artifacts": {},
    }
    write_json(output / "run.json", state)
    if prep.get("arms", {}).get(corpus_id, {}).get("status") != "ready":
        state.update({"status": "never_ran", "reason": "preparation arm was not ready"})
        write_json(output / "run.json", state)
        return state
    started, env = time.monotonic(), cli_env(ROOT)
    try:
        state["artifacts"].update(archive_pre_run(corpus_id, output))
        extract = invoke("extract-full", ["sovereign", "enrich", "extract", corpus_id, "--full"], output / "logs/extract-full.log", env=env)
        state["steps"].append(record_step(extract))
        paths = runtime_paths(corpus_id)
        state["artifacts"]["phase1_checkpoint"] = copy_once(paths["checkpoint"], output / "artifacts/phase1-checkpoint.jsonl")
        state["artifacts"]["phase1_tokens"] = copy_once(paths["tokens"], output / "artifacts/phase1-tokens.json")
        checkpoint = output / "artifacts/phase1-checkpoint.jsonl"
        tokens = output / "artifacts/phase1-tokens.json"
        state["phase1_failures"] = read_checkpoint(checkpoint)
        state["phase1_tokens"] = token_snapshot(tokens)
        if extract.get("exit_code") != 0:
            state.update({"status": "failed", "failed_step": "extract-full", "exit_code": extract.get("exit_code")})
        else:
            finalize = invoke("finalize", ["sovereign", "enrich", "extract", corpus_id, "--finalize"], output / "logs/finalize.log", env=env)
            state["steps"].append(record_step(finalize))
            if finalize.get("exit_code") != 0:
                state.update({"status": "failed", "failed_step": "finalize", "exit_code": finalize.get("exit_code")})
            else:
                state["artifacts"]["questions"] = copy_once(paths["questions"], output / "artifacts/questions.json")
                argv = ["sovereign", "enrich", "build", corpus_id]
                for skipped in ("extract", "configure", "tensions", "gaps"):
                    argv.extend(["--skip", skipped])
                build = invoke("build", argv, output / "logs/build.log", env=env)
                state["steps"].append(record_step(build))
                for name, dest in (("atoms", "atlas/atoms.json"), ("edges", "atlas/edges.json")):
                    state["artifacts"][name] = copy_once(paths[name], output / f"artifacts/{dest}")
                if build.get("exit_code") != 0:
                    state.update({"status": "failed", "failed_step": "build", "exit_code": build.get("exit_code")})
                elif any(state["artifacts"].get(name, {}).get("status") != "copied" for name in ("questions", "atoms", "edges")):
                    state.update({"status": "could_not_judge", "reason": "production build exited zero but an expected artifact is absent"})
                else:
                    state["status"] = "complete"
        selected = source_ids(app, load_inputs())
        state["document_read"] = read_status_report(paths["questions"], app, selected, ROOT / f"inputs/{app}")
        state["phase_cost_log_lines"] = phase_cost_lines(output)
    except Exception as exc:
        state.update({"status": "failed", "error": f"{type(exc).__name__}: {exc}"})
    state["scoped_build_wall_seconds"] = round(time.monotonic() - started, 3)
    state["completed_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    write_json(output / "run.json", state)
    return state


def load_inputs():
    return json.loads((ROOT / "inputs/source-selection.json").read_text(encoding="utf-8"))


def run_trials(only_arm=None):
    prep_path = ROOT / "preparation.json"
    if not prep_path.is_file():
        raise FileNotFoundError(f"prepare first: {prep_path}")
    prereg = load_prereg(ROOT / "reader-trial-prereg.toml")
    prep = json.loads(prep_path.read_text(encoding="utf-8"))
    arms = [arm for arm in arm_specs(prereg) if only_arm is None or arm["corpus_id"] == only_arm]
    statuses = []
    for arm in arms:
        result = run_arm(arm, prereg, prep)
        row = {"corpus_id": arm["corpus_id"], "status": result["status"], "wall_seconds": result.get("scoped_build_wall_seconds")}
        statuses.append(row)
        print(json.dumps(row))
    return 0 if all(row["status"] == "complete" for row in statuses) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("prepare", help="freeze data and binaries, then validate/publish/index/init")
    run_parser = sub.add_parser("run", help="serial production extract/finalize/build; launch explicitly")
    score_parser = sub.add_parser("score", help="score archived results without rerunning production")
    for child in (run_parser, score_parser):
        child.add_argument("--arm", choices=ARM_IDS, help="one arm; default is all arms in preregistered order")
    args = parser.parse_args()
    if args.command == "prepare":
        return prepare()
    if args.command == "run":
        return run_trials(args.arm)
    if not (ROOT / "preparation.json").is_file():
        raise FileNotFoundError(f"prepare first: {ROOT / 'preparation.json'}")
    return score_trials(invoke, args.arm, ROOT, APP)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:
        print(f"reader trial could not continue: {type(exc).__name__}: {exc}", file=sys.stderr)
        sys.exit(2)
