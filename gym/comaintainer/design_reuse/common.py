#!/usr/bin/env python3
"""Shared machinery for the design-reuse lane of the comaintainer gym.

One home for: case loading, git verification, leakage checks, prompt
rendering, output schemas and deterministic proposal scoring. `validate.py`
and `replay.py` import this; nothing here calls a model.

Provenance rules (see README.md):
- Each case carries a BASE and OUTCOME revision. BASE is the outcome's
  first parent or a verified ancestor (artifact-introduction), stated per
  case. This is reconstructed replay, not the original task transcript.
- The request side (requirement, constraints) and the dossier (candidate
  surfaces at BASE) are all a candidate may see. The oracle (expected
  disposition, correction quote, bad alternatives) is answer-side only.
- Candidate excerpts are verified verbatim against the BASE tree; a failed
  excerpt is a bank defect, never a scoring input.
"""
from __future__ import annotations

import copy
import json
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))          # gym/comaintainer, for markers

import markers as M  # noqa: E402
import repo_access as P

REPO = HERE.parents[2]
CASES = HERE / "cases.jsonl"


# ---- git -----------------------------------------------------------------


def git(*args: str) -> tuple[int, str, str]:
    p = subprocess.run(["git", *args], capture_output=True, text=True, cwd=REPO)
    return p.returncode, p.stdout.strip(), p.stderr.strip()


def resolves(sha: str) -> bool:
    rc, _, _ = git("cat-file", "-e", f"{sha}^{{commit}}")
    return rc == 0


def is_ancestor(ancestor: str, descendant: str) -> bool:
    rc, _, _ = git("merge-base", "--is-ancestor", ancestor, descendant)
    return rc == 0


def is_first_parent(base: str, outcome: str) -> bool:
    _, parent, _ = git("rev-parse", f"{outcome}^")
    return parent == base


def blob_at(base: str, path: str) -> str | None:
    p = subprocess.run(["git", "show", f"{base}:{path}"], capture_output=True,
                       text=True, cwd=REPO)
    return p.stdout if p.returncode == 0 else None


def source_window(case: dict, candidate: dict) -> dict:
    """Read the actual declared window, never substitute a short excerpt."""
    base, path = case["provenance"]["base_sha"], candidate["path"]
    if not re.fullmatch(r"[0-9a-f]{40}", base):
        raise ValueError("source revision must be a full commit id")
    if not path or path.startswith("/") or ".." in Path(path).parts or "\n" in path:
        raise ValueError("source path must stay inside the historical tree")
    lines = candidate.get("lines")
    if (not isinstance(lines, list) or len(lines) != 2
            or any(type(n) is not int for n in lines)):
        raise ValueError("source window requires two integer line coordinates")
    start, end = lines
    observed = P.read_window(base, path, start, end)
    text = observed["code"]
    excerpt = candidate.get("excerpt", "")
    if not excerpt.strip() or norm(excerpt) not in norm(text):
        raise ValueError(f"excerpt not present in declared window: {path}:{start}-{end}")
    return {"candidate": candidate["id"], "base": base, "path": path,
            "lines": lines, "blob": observed["git_blob"],
            "sha256": observed["window_sha256"], "source_sha256": observed["source_sha256"],
            "receipt": observed["receipt_id"], "text": text}


# ---- text ----------------------------------------------------------------


def norm(s: str) -> str:
    """Case-insensitive, whitespace-collapsed, punctuation-light normal form.

    Backticks, asterisks and markdown leaders are presentation; two texts
    that differ only in those are the same text for matching purposes."""
    s = s.replace("`", "").replace("*", "")
    s = re.sub(r"[ \t]+", " ", s)
    return re.sub(r"\s+", " ", s).strip().lower()


# ---- bank -----------------------------------------------------------------


def load_cases(path: Path = CASES) -> list[dict]:
    return M.read_bank(path)


def request_text(case: dict) -> str:
    parts = [case["request"]["requirement"], *case["request"]["constraints"]]
    for cand in case["dossier"]["candidates"]:
        try:
            parts.append(source_window(case, cand)["text"])
        except ValueError:
            parts.append(cand.get("excerpt", ""))
    return "\n".join(parts)


def verify_case(case: dict) -> list[str]:
    """Every deterministic check a case must pass, as a list of failures."""
    cid = case.get("id", "?")
    bad: list[str] = []
    prov = case.get("provenance", {})
    base, outcome = prov.get("base_sha", ""), prov.get("outcome_sha", "")
    if not resolves(base):
        bad.append(f"{cid}: base_sha does not resolve: {base!r}")
    if not resolves(outcome):
        bad.append(f"{cid}: outcome_sha does not resolve: {outcome!r}")
    if base and outcome and resolves(base) and resolves(outcome):
        if not is_ancestor(base, outcome):
            bad.append(f"{cid}: base is not an ancestor of outcome")
        elif not is_first_parent(base, outcome):
            # Allowed (artifact-introduction reconstructions), reported.
            pass
    seen: set[str] = set()
    for cand in case.get("dossier", {}).get("candidates", []):
        cand_id = cand.get("id", "?")
        if cand_id in seen:
            bad.append(f"{cid}: duplicate candidate id {cand_id!r}")
        seen.add(cand_id)
        path = cand.get("path", "")
        if not path or path.startswith("/"):
            bad.append(f"{cid}/{cand_id}: candidate path must be repo-relative: {path!r}")
            continue
        try:
            source_window(case, cand)
        except ValueError as exc:
            bad.append(f"{cid}/{cand_id}: {exc}")
        facts = cand.get("facts") or {}
        if not isinstance(facts.get("usable"), bool) or not isinstance(facts.get("serves"), bool):
            bad.append(f"{cid}/{cand_id}: facts.usable and facts.serves must both be booleans")
        # The one mechanically-groundable fact: a pub(crate) excerpt cannot be
        # usable from outside its crate. Everything else is pre-registered and
        # inherits the case's label tier.
        elif facts["usable"] and "pub(crate)" in (cand.get("excerpt") or ""):
            bad.append(f"{cid}/{cand_id}: usable=true but the excerpt shows pub(crate)")
    oracle = case.get("oracle", {})
    for field in ("expected_disposition", "acceptable_owner_identities",
                  "acceptable_path_identities", "correction_quote", "bad_alternatives"):
        if not oracle.get(field):
            bad.append(f"{cid}: oracle.{field} is empty")
    for bait in oracle.get("bad_alternatives", []):
        if not bait.get("tokens"):
            bad.append(f"{cid}: bad alternative {bait.get('selection', '?')!r} has no tokens")
    # Leakage: answer-side material must not appear on the request side.
    req = request_text(case)
    quote = oracle.get("correction_quote", "")
    overlap = M.shingles(quote) & M.shingles(req)
    if overlap:
        bad.append(f"{cid}: correction quote leaks into request: {next(iter(overlap))}")
    if outcome and outcome in req:
        bad.append(f"{cid}: outcome sha appears in request text")
    return bad


# ---- splits --------------------------------------------------------------


def assign_splits(cases: list[dict]) -> dict[str, str]:
    """Deterministic dev/holdout assignment, recomputed never stored.

    Stratum = label (historic_operator vs proposed_referee_needed);
    within a stratum, ids sorted, every third goes to holdout (the house
    rule, markers.split_of as the one formula). Family is deliberately
    NOT the stratum: eleven families give eleven singletons and a bank
    with no holdout at all."""
    out: dict[str, str] = {}
    groups: dict[str, list[dict]] = {}
    for case in cases:
        label = case.get("provenance", {}).get("label", "?")
        groups.setdefault(label, []).append(case)
    for members in groups.values():
        for i, case in enumerate(sorted(members, key=lambda c: c["id"])):
            out[case["id"]] = M.split_of(case["family"],
                                         case["provenance"]["label"],
                                         case["oracle"]["expected_disposition"][0], i)
    return out


# ---- prompts and schemas -------------------------------------------------

DISPOSITIONS = ("USE", "EXTEND", "ADAPT", "ADD")

LEGEND = """DISPOSITION vocabulary:
- USE: the capability already exists; adopt it, name it, add no new structure.
- EXTEND: the existing owner gains the capability; name the owner and the new surface.
- ADAPT: an existing artifact is reused through a new adapter or call path.
- ADD: genuinely new capability is needed after the surveyed candidates cannot serve; justify why.
Prefer the smallest new structure. A new store, pass, service or taxonomy is an ADD and must be justified."""

INTRO = ("You are a senior engineer doing technical design in a large, "
         "seven-month-old repository (commonwealth-ai). Real developers find the "
         "existing owner and extend it; they treat every task as the first thing "
         "ever built only when the survey proves nothing serves. New stores, "
         "passes, services and taxonomies are expensive.")


def render_dossier(case: dict) -> str:
    lines = []
    for cand in case["dossier"]["candidates"]:
        window = cand.get("source_window") or source_window(case, cand)
        lines.append(f"[{cand['id']}] {cand['path']}:{window['lines'][0]}-{window['lines'][1]}")
        lines.append(window["text"])
    return "\n".join(lines)


def build_prompt(case: dict, condition: str) -> str:
    parts = [INTRO, LEGEND]
    parts.append("TASK:\n" + case["request"]["requirement"])
    parts.append("CONSTRAINTS:\n" + "\n".join(f"- {c}" for c in case["request"]["constraints"]))
    if condition in ("B", "C"):
        parts.append("CANDIDATE EXISTING SURFACES (from the repository at the recorded revision):\n"
                     + render_dossier(case))
    if condition == "C":
        parts.append(
            "DESIGN PROTOCOL (binding):\n"
            "- Ground the design in the candidates above; evidence must list the candidate ids you rely on.\n"
            "- new_components must enumerate EVERY store, pass, service, type or taxonomy you propose to create; empty if none.\n"
            "- limits must state what the existing surfaces cannot do for this task.\n"
            "- If the candidates fully serve the task, choose USE and add nothing.")
    parts.append("Return one JSON object now.")
    return "\n\n".join(parts)


def schema_for(case: dict, condition: str) -> dict:
    props: dict = {
        "disposition": {"type": "string", "enum": list(DISPOSITIONS)},
        "owner": {"type": "string"},
        "seam": {"type": "string"},
        "delta": {"type": "string", "minLength": 40},
        "new_components": {"type": "array", "items": {"type": "string"}},
        "limits": {"type": "string", "minLength": 20},
        "evidence": {"type": "array", "items": {"type": "string"}},
    }
    required = list(props)
    if condition == "C":
        ids = [c["id"] for c in case["dossier"]["candidates"]]
        # Grounding is STRUCTURAL under C: the decoder can only emit
        # candidate ids, so "I cited something" cannot be a free-text claim
        # (principle 10 — an enumerated set is a type, not a request). The
        # escape names the genuine "none of these serve" case without
        # letting arbitrary prose through.
        props["evidence"]["items"] = {"type": "string", "enum": ids + ["none-of-these"]}
        props["evidence"]["minItems"] = 1
        props["limits"]["minLength"] = 20
        props["delta"]["minLength"] = 40
    return {"type": "object", "properties": props, "required": required,
            "additionalProperties": False}


# ---- scoring -------------------------------------------------------------


def score_proposal(case: dict, proposal: dict, condition: str) -> dict:
    """Deterministic metrics over one parsed proposal. Semantic adequacy
    remains a refereed question; these are the mechanical facts.

    Selection metrics read the CHOICE fields (owner, seam, delta,
    new_components) only. The evidence list records what was surveyed —
    including surfaces considered and rejected — so counting it as
    "found" would let a proposal that names `IndexSource` in order to
    reject it read as a hit (observed in the 2026-10-05 run; fixed by
    rescore)."""
    oracle = case["oracle"]
    proposal = copy.deepcopy(proposal or {})
    choice = norm(" ".join(
        [str(proposal.get(k, "")) for k in ("owner", "seam", "delta")]
        + [str(x) for x in proposal.get("new_components", [])]))
    expected = oracle["expected_disposition"]
    home_ids = oracle["acceptable_owner_identities"] + oracle.get("acceptable_seam_identities", [])
    bait_hits = [b["selection"] for b in oracle["bad_alternatives"]
                 if any(norm(t) in choice for t in b["tokens"])]
    dossier_ids = {c["id"] for c in case["dossier"]["candidates"]}
    evidence = [str(e).strip().strip("[]") for e in (proposal.get("evidence") or [])]
    return {
        "disposition": proposal.get("disposition"),
        "disposition_match": proposal.get("disposition") in expected,
        "home_match": any(norm(i) in choice for i in home_ids),
        "path_match": any(norm(p) in choice for p in oracle["acceptable_path_identities"]),
        "new_components": len(proposal.get("new_components") or []),
        "new_component_list": proposal.get("new_components") or [],
        "evidence_grounded": (bool(evidence) and all(e in dossier_ids for e in evidence))
                              if condition in ("C", "D") else None,
        "bait_hits": bait_hits,
    }


def aggregate(rows: list[dict]) -> dict:
    out: dict[str, dict] = {}
    for row in rows:
        d = out.setdefault(row["condition"], {
            "n": 0, "malformed": 0, "could_not_judge": 0, "disposition_match": 0,
            "home_match": 0, "path_match": 0, "new_components": 0, "bait_rows": 0,
            "evidence_grounded": 0, "evidence_required": 0,
            "refusals": 0, "wrong_observations": 0,
            "failed": 0, "never_ran": 0,
        })
        d["n"] += 1
        d["refusals"] += int(row.get("refusals") or 0)
        d["wrong_observations"] += int(row.get("wrong_observations") or 0)
        if row.get("verdict") in ("failed", "never-ran"):
            d["failed" if row["verdict"] == "failed" else "never_ran"] += 1
            continue
        if row.get("verdict") == "malformed":
            d["malformed"] += 1
            continue
        if row.get("verdict") == "could-not-judge":
            d["could_not_judge"] += 1
            continue
        m = row.get("metrics") or {}
        d["disposition_match"] += bool(m.get("disposition_match"))
        d["home_match"] += bool(m.get("home_match"))
        d["path_match"] += bool(m.get("path_match"))
        d["new_components"] += int(m.get("new_components", 0))
        d["bait_rows"] += bool(m.get("bait_hits"))
        if row["condition"] in ("C", "D"):
            d["evidence_required"] += 1
            d["evidence_grounded"] += bool(m.get("evidence_grounded"))
    return out


def render_aggregate(agg: dict) -> str:
    lines = ["condition  n  parsed  disp  home  path  new_comp  bait_rows  evidence  refused  wrongobs"]
    for cond in sorted(agg):
        d = agg[cond]
        parsed = d["n"] - sum(d[k] for k in ("malformed", "could_not_judge", "failed", "never_ran"))
        ev = f"{d['evidence_grounded']}/{d['evidence_required']}" if d["evidence_required"] else "-"
        lines.append(f"{cond:>9}  {d['n']}  {parsed:>6}  {d['disposition_match']:>4}  "
                     f"{d['home_match']:>4}  {d['path_match']:>4}  {d['new_components']:>8}  "
                     f"{d['bait_rows']:>9}  {ev:>8}  {d['refusals']:>7}  {d['wrong_observations']:>8}")
    return "\n".join(lines)
