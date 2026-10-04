# SPDX-License-Identifier: AGPL-3.0-or-later
"""Shared workspace mechanics for the SWE-bench Verified arms.

Every arm reduces to one contract:

    materialize(instance) -> Path        # repo at base_commit, clean
    <arm does whatever it does>
    extract_patch(Path)   -> str         # unified diff, tests excluded by the harness

Arms differ only in what happens between those two calls. That seam is
what makes the ablation honest: `bare-metal`, `native`, `mini-swe-agent`
and `comaintainer` all get an identical starting tree and are read the
same way.

The gold `patch` field is deliberately NOT carried into the working
instance record — see `prepare.py`. An arm that can see the answer is
not measuring anything.
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPOS = ROOT / "repos"
WORK = ROOT / "work"
INSTANCES = ROOT / "instances.jsonl"

# Operator preference 2026-08-18; `docker` is drop-in.
ENGINE = "podman"
# SWE-bench publishes x86_64 images only. They run under emulation on
# arm64 — verified on this host, ~3.5s for a single-test run.
PLATFORM = "linux/amd64"


class WorkspaceError(RuntimeError):
    pass


def sh(cmd: list[str], cwd: Path | None = None, check: bool = True, timeout: int = 900) -> str:
    r = subprocess.run(
        cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout
    )
    if check and r.returncode != 0:
        raise WorkspaceError(f"{' '.join(cmd[:4])}… exit {r.returncode}\n{r.stderr[-2000:]}")
    return r.stdout


@dataclass(frozen=True)
class Instance:
    instance_id: str
    repo: str
    base_commit: str
    problem_statement: str
    version: str
    difficulty: str

    @property
    def slug(self) -> str:
        return self.repo.replace("/", "__")

    @property
    def bare(self) -> Path:
        return REPOS / f"{self.slug}.git"

    @property
    def image(self) -> str:
        """The official per-instance image. A docker tag cannot carry a
        double underscore, so SWE-bench encodes `__` as `_1776_`."""
        return (
            "docker.io/swebench/sweb.eval.x86_64."
            f"{self.instance_id.replace('__', '_1776_')}:latest"
        )

    def verify_cmd(self, workdir: Path, inner: str = "python -m pytest -x -q") -> str:
        """Run `inner` inside the instance image with `workdir` mounted
        over /testbed — the footing every published number is measured on."""
        return (
            f"{ENGINE} run --rm --platform {PLATFORM} "
            f"-v {workdir}:/testbed {self.image} bash -lc 'cd /testbed && {inner}'"
        )


def load_instances(path: Path = INSTANCES) -> list[Instance]:
    if not path.exists():
        raise WorkspaceError(f"{path} missing — run prepare.py first")
    out = []
    for line in path.read_text().splitlines():
        if line.strip():
            d = json.loads(line)
            out.append(Instance(**{k: d[k] for k in Instance.__dataclass_fields__}))
    return out


def ensure_bare(inst: Instance) -> Path:
    """One bare clone per repo, shared by every instance and every arm."""
    if inst.bare.exists():
        return inst.bare
    REPOS.mkdir(parents=True, exist_ok=True)
    sh(
        ["git", "clone", "--bare", f"https://github.com/{inst.repo}.git", str(inst.bare)],
        timeout=3600,
    )
    return inst.bare


def materialize(inst: Instance, arm: str, force: bool = False) -> Path:
    """The instance's REAL environment at base_commit, for (arm, instance).

    Copied out of the official image rather than cloned from git: every
    published SWE-bench number is produced by an agent working inside
    that image, with dependencies installed and the suite runnable. A
    bare checkout is a strictly harder, non-comparable variant — see
    `../README.md`. The copy carries `.git` and `.egg-info`, so the same
    directory mounts back over /testbed for verification.
    """
    dest = WORK / arm / inst.instance_id
    if dest.exists() and not force:
        sh(["git", "checkout", "--force", inst.base_commit], cwd=dest)
        sh(["git", "clean", "-fdx"], cwd=dest)
        return dest
    if dest.exists():
        sh(["rm", "-rf", str(dest)])
    dest.mkdir(parents=True, exist_ok=True)

    cid = sh([ENGINE, "create", "--platform", PLATFORM, inst.image, "sleep", "1"],
             timeout=3600).strip()
    try:
        sh([ENGINE, "cp", f"{cid}:/testbed/.", str(dest)], timeout=1800)
    finally:
        sh([ENGINE, "rm", "-f", cid], check=False)
    # The image sits at its build commit, not the instance's.
    sh(["git", "checkout", "--detach", "--quiet", inst.base_commit], cwd=dest)
    return dest


def extract_patch(workdir: Path) -> str:
    """Everything the arm changed, as a unified diff against base_commit.

    Staged via `git add -A` so new files are captured; .gitignore still
    applies, so build artifacts stay out. Edits to test files are not
    filtered here — the official harness force-restores test files before
    applying its own `test_patch`, so they cannot buy a false pass.
    """
    sh(["git", "add", "-A"], cwd=workdir)
    return sh(["git", "diff", "--cached", "--no-color"], cwd=workdir)


def write_prediction(arm: str, inst: Instance, patch: str, model: str) -> None:
    out = ROOT / "preds" / arm
    out.mkdir(parents=True, exist_ok=True)
    (out / f"{inst.instance_id}.json").write_text(
        json.dumps(
            {
                "instance_id": inst.instance_id,
                "model_name_or_path": f"{arm}:{model}",
                "model_patch": patch,
            }
        )
        + "\n"
    )
