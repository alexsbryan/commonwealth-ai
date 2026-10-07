"""Run inputs are immutable records; legacy logs cannot be silently rescored."""
import copy
import hashlib
import json
from pathlib import Path

import common as C

VERSION = "source-window-v3"


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def instruments():
    names = ("common.py", "replay.py", "historical.py", "repo_access.py", "contracts.py", "frozen.py", "loop.py")
    return {name: hashlib.sha256((C.HERE / name).read_bytes()).hexdigest() for name in names}


def record(run_dir, cases, settings):
    material = copy.deepcopy(cases)
    for case in material:
        for cand in case["dossier"]["candidates"]:
            cand["source_window"] = C.source_window(case, cand)
    payload = {"version": VERSION, "cases": material,
               "splits": C.assign_splits(cases), "settings": settings,
               "instruments": instruments()}
    (run_dir / "inputs.json").write_text(json.dumps(payload, indent=2) + "\n")
    (run_dir / "inputs.sha256").write_text(digest(payload) + "\n")
    schemas = {c["id"]: {arm: C.schema_for(c, arm) for arm in ("A", "B", "C")} for c in cases}
    (run_dir / "schemas.json").write_text(json.dumps(schemas, indent=2) + "\n")
    for name in payload["instruments"]:
        target = run_dir / "instrument" / name
        target.parent.mkdir(exist_ok=True)
        target.write_bytes((C.HERE / name).read_bytes())
    return payload


def read(run_dir):
    path = run_dir / "inputs.json"
    if not path.exists():
        raise ValueError("could-not-judge: legacy run has no frozen inputs; current bank is not a substitute")
    payload = json.loads(path.read_text())
    if digest(payload) != (run_dir / "inputs.sha256").read_text().strip():
        raise ValueError("could-not-judge: frozen input digest mismatch")
    if payload["instruments"] != instruments():
        raise ValueError("could-not-judge: scorer changed; declare a new scoring version before rescoring")
    return payload
