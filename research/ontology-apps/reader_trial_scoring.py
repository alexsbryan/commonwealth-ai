"""Read-only cache diagnostics and existing scorer orchestration."""
import collections
import importlib
import json
import pathlib
import re
import sys

from reader_trial_data import DATA, ROOT, cli_env, load_prereg, source_ids, write_json


TOKEN_LINE = re.compile(r"\b(?:phase|calls?|prompt_tokens|completion_tokens|total_tokens|tokens)\b", re.I)


def runtime_paths(corpus_id):
    enrich, index = DATA / "enrichment" / corpus_id, DATA / "indexes" / corpus_id
    return {
        "config": enrich / "config.json",
        "chapters": index / "chapters.json",
        "checkpoint": enrich / "runs/_phase1_checkpoint.jsonl",
        "tokens": enrich / "_tokens.json",
        "questions": enrich / "cache/questions.json",
        "atoms": index / "atlas/atoms.json",
        "edges": index / "atlas/edges.json",
    }


def chapter_count(path):
    path = pathlib.Path(path)
    if not path.is_file():
        return None
    chapters = json.loads(path.read_text(encoding="utf-8")).get("chapters")
    return len(chapters) if isinstance(chapters, list) else None


def copy_once(source, destination):
    source, destination = pathlib.Path(source), pathlib.Path(destination)
    if destination.exists():
        raise FileExistsError(f"refusing to replace archived artifact: {destination}")
    if not source.is_file():
        return {"status": "not_found", "source": str(source), "archive": str(destination)}
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(source.read_bytes())
    destination.chmod(0o444)
    from reader_trial_data import sha256_file
    return {"status": "copied", "source": str(source), "archive": str(destination), "sha256": sha256_file(destination)}


def read_checkpoint(path):
    path = pathlib.Path(path)
    if not path.is_file():
        return {"status": "not_recorded", "entry_count": None, "failure_count": None, "failure_chapter_ids": []}
    counts, failed = collections.Counter(), []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip():
            row = json.loads(line)
            kind = str(row.get("kind", "unknown"))
            counts[kind] += 1
            if kind != "success":
                failed.append(str(row.get("chapter_id", "<missing chapter_id>")))
    return {
        "status": "observed", "entry_count": sum(counts.values()),
        "entry_kinds": dict(counts), "failure_count": len(failed), "failure_chapter_ids": failed,
    }


def document_read_outcomes(cache):
    found, duplicates = {}, collections.defaultdict(set)
    refused = collections.Counter()
    for chapter in cache.get("questions_by_chapter") or []:
        extraction = chapter.get("section_extraction") or {}
        read = extraction.get("document_read")
        if not isinstance(read, dict):
            continue
        for item in read.get("documents") or []:
            if not isinstance(item, dict):
                continue
            if item.get("document_id") is not None and item.get("status") is not None:
                doc_id, status = str(item["document_id"]), str(item["status"])
                found[doc_id] = status
                duplicates[doc_id].add(status)
            for refusal in item.get("refused") or []:
                if isinstance(refusal, dict):
                    refused[str(refusal.get("kind", "<missing>"))] += 1
    conflicts = {doc_id: sorted(states) for doc_id, states in duplicates.items() if len(states) > 1}
    if conflicts:
        raise ValueError(f"document_read contains conflicting duplicate statuses: {conflicts}")
    return found, dict(refused)


def read_status_report(cache_path, application, selected, input_root):
    cache_path = pathlib.Path(cache_path)
    if not cache_path.is_file():
        return {"status": "not_recorded", "reason": "canonical questions cache absent", "selected_document_statuses": {}}
    cache = json.loads(cache_path.read_text(encoding="utf-8"))
    outcomes, refused = document_read_outcomes(cache)
    if application == "ward":
        manifest = json.loads((pathlib.Path(input_root) / "manifest.json").read_text(encoding="utf-8"))
        aliases = {}
        for entry in manifest:
            rel = str(entry["path"])
            if rel in selected:
                aliases[rel] = rel
                aliases.update({str(mid): rel for mid in entry.get("message_ids", [])})
    else:
        aliases = {doc_id: doc_id for doc_id in selected}
        staged = pathlib.Path(input_root) / "raw/documents.jsonl"
        if staged.is_file():
            for line in staged.read_text(encoding="utf-8").splitlines():
                if not line.strip():
                    continue
                row = json.loads(line)
                doc_id = str(row.get("id"))
                if doc_id not in selected:
                    continue
                aliases[doc_id] = doc_id
                url = row.get("url")
                if isinstance(url, str) and url:
                    aliases.setdefault(url, doc_id)
    mapped = {aliases[doc_id]: status for doc_id, status in outcomes.items() if doc_id in aliases}
    return {
        "status": "observed" if outcomes else "not_recorded",
        "source": str(cache_path), "outcome_count": len(outcomes),
        "refused_claims": refused,
        "refused_claim_count": sum(refused.values()),
        "selected_document_statuses": mapped,
        "selected_status_counts": dict(collections.Counter(mapped.values())),
        "selected_documents_without_document_read_status": sorted(set(selected) - set(mapped)),
        "document_read_ids_not_mapped_to_selected_sources": sorted(set(outcomes) - set(aliases)),
    }


def token_snapshot(path):
    path = pathlib.Path(path)
    if not path.is_file():
        return {"status": "not_recorded"}
    data = json.loads(path.read_text(encoding="utf-8"))
    keys = ("corpus_id", "calls", "prompt_tokens", "completion_tokens", "total_tokens", "started_at_ms", "updated_at_ms")
    return {"status": "observed", **{key: data.get(key) for key in keys}}


def phase_cost_lines(run_dir):
    result = {}
    for path in sorted((pathlib.Path(run_dir) / "logs").glob("*.log")):
        matched = [line for line in path.read_text(encoding="utf-8").splitlines() if TOKEN_LINE.search(line)]
        if matched:
            result[path.stem] = matched
    return result


def load_score_helpers(app_root):
    sys.path.insert(0, str(app_root))
    return importlib.import_module("support.score"), importlib.import_module("ward.score")


def score_uv(corpus_id, score_dir, support_score, input_root, invoke):
    support_score.ROOT = input_root
    gold, ambiguous, none, case_count = support_score.load_gold("tune")
    docs = [json.loads(line) for line in (input_root / "raw/documents.jsonl").read_text(encoding="utf-8").splitlines() if line.strip()]
    atlas_path = DATA / "indexes" / corpus_id / "atlas/atoms.json"
    predicted, atlas_read = support_score.atlas_pred(str(atlas_path), "case_membership")
    predicted = {str(key): str(value) for key, value in predicted.items()}
    scoped, scope = support_score.evaluation_scope(predicted, gold, ambiguous, none, docs, "tune")
    pred_path, gold_path = score_dir / "predicted.json", score_dir / "gold.json"
    write_json(pred_path, scoped, readonly=True)
    write_json(gold_path, gold, readonly=True)
    result = invoke(
        "er-score", ["sovereign", "bench", "er-score", str(pred_path), str(gold_path)],
        score_dir / "er-score.log", env=cli_env(ROOT),
    )
    if result.get("exit_code") != 0:
        return {"status": "could_not_judge", "reason": "bench er-score failed", "exit_code": result.get("exit_code"), "atlas_read": atlas_read, **scope}
    metrics = json.loads(result["stdout"])
    if "recovery_b_cubed" not in metrics:
        return {"status": "could_not_judge", "reason": "bench er-score omitted recovery_b_cubed", "atlas_read": atlas_read, **scope}
    return {
        "status": "scored", "membership_kind": "case_membership", "gold_cases": case_count,
        "gold_documents": len(gold), "ambiguous_gold_excluded": len(ambiguous),
        "known_no_case_documents": len(none),
        "membership_coverage": round(len(set(gold) & set(scoped)) / len(gold), 6) if gold else None,
        "gold_unpredicted": len(set(gold) - set(scoped)), "atlas_read": atlas_read, "scope": scope,
        "metrics": metrics, "artifacts": {"predicted": str(pred_path), "gold": str(gold_path)},
    }


def score_ward(corpus_id, ward_score, input_root):
    ward_score.WARD = input_root
    secfiles = ward_score.section_files(corpus_id)
    atoms_path = DATA / "indexes" / corpus_id / "atlas/atoms.json"
    atoms = json.loads(atoms_path.read_text(encoding="utf-8"))["atoms"]
    entities, claims = ward_score.load_atlas(atoms, secfiles)
    gold = ward_score.load_gold(input_root / "gold")
    scope = set(gold["files"])
    result = ward_score.score(gold, entities, claims, scope)
    return {
        "status": "scored", "scoped_source_file_count": len(scope), "scoped_source_files": sorted(scope),
        "deal_coverage": result["deals"], "stage_coverage": result["stage"],
        "commitment_coverage": result["commitments"], "false_claim_diagnostics": result["made_up"],
        "unmatched_deal_atom_count": result["deal_atoms_unmatched"],
        "unscoped_people_diagnostics": {key: result[key] for key in ("people", "companies", "contacts")},
        "other_diagnostics": {key: value for key, value in result.items() if key not in {
            "people", "companies", "contacts", "deals", "stage", "commitments", "made_up"
        }},
        "ward_mixed_record_diagnostic": "not emitted by the existing Ward scorer",
    }


def nested(value, path):
    for part in path.split("."):
        if not isinstance(value, dict) or part not in value:
            return None
        value = value[part]
    return value


def compare_scores(legacy, narrow, application):
    if legacy.get("status") != "scored" or narrow.get("status") != "scored":
        return {"status": "could_not_judge", "reason": "both arms need scored outputs"}
    if application == "uv":
        primary = ["membership_coverage", "metrics.recovery_b_cubed.f1"]
        controls = ["scope.no_case_documents_placed", "atlas_read.documents_in_several_records"]
    else:
        primary = ["deal_coverage.recall", "stage_coverage.recall"]
        controls = ["false_claim_diagnostics.stage_updates", "false_claim_diagnostics.commitments", "unmatched_deal_atom_count"]
    paths = primary + controls
    deltas = {path: {"legacy": nested(legacy, path), "narrow": nested(narrow, path)} for path in paths}
    improved = all(deltas[path]["legacy"] is not None and deltas[path]["narrow"] is not None and deltas[path]["narrow"] > deltas[path]["legacy"] for path in primary)
    controls_ok = all(deltas[path]["legacy"] is not None and deltas[path]["narrow"] is not None and deltas[path]["narrow"] <= deltas[path]["legacy"] for path in controls)
    return {"status": "compared", "primary_improved": improved, "controls_not_worse": controls_ok, "preregistered_arm_bar_met": improved and controls_ok, "deltas": deltas}


def score_trials(invoke, only_arm=None, root=ROOT, app_root=None):
    root = pathlib.Path(root)
    prereg = load_prereg(root / "reader-trial-prereg.toml")
    prep = json.loads((root / "preparation.json").read_text(encoding="utf-8"))
    support, ward = load_score_helpers(app_root)
    staged = json.loads((root / "inputs/source-selection.json").read_text(encoding="utf-8"))
    results, failed = {}, False
    for arm in prereg["arm"]:
        corpus_id, app = arm["corpus_id"], arm["application"]
        if only_arm is not None and corpus_id != only_arm:
            continue
        run_path, out = root / "runs" / corpus_id / "run.json", root / "scores" / corpus_id
        if out.exists():
            raise FileExistsError(f"score already archived; refusing overwrite: {out}")
        out.mkdir(parents=True)
        if not run_path.is_file():
            result = {"status": "never_ran", "reason": "no archived run record"}
        else:
            run = json.loads(run_path.read_text(encoding="utf-8"))
            if run.get("status") != "complete":
                result = {"status": "could_not_judge", "reason": f"run status is {run.get('status')}", "run_status": run.get("status")}
            else:
                try:
                    result = score_uv(corpus_id, out, support, root / "inputs/uv", invoke) if app == "uv" else score_ward(corpus_id, ward, root / "inputs/ward")
                except Exception as exc:
                    result = {"status": "could_not_judge", "reason": f"{type(exc).__name__}: {exc}"}
        run = json.loads(run_path.read_text(encoding="utf-8")) if run_path.is_file() else {}
        result.update({
            "corpus_id": corpus_id, "application": app,
            "document_read": run.get("document_read", {"status": "not_recorded"}),
            "phase1_failures": run.get("phase1_failures", {"status": "not_recorded"}),
            "phase1_token_snapshot": run.get("phase1_tokens", {"status": "not_recorded"}),
            "scoped_build_wall_seconds": run.get("scoped_build_wall_seconds"),
            "phase_cost_log_lines": run.get("phase_cost_log_lines", {}),
            "indexed_vs_selected": prep.get("arms", {}).get(corpus_id, {}).get("index", {"status": "not_recorded"}),
        })
        write_json(out / "score.json", result, readonly=True)
        results[corpus_id] = result
        failed |= result["status"] != "scored"
        print(json.dumps({"corpus_id": corpus_id, "score_status": result["status"], "artifact": str(out / "score.json")}))
    if only_arm is None:
        by_app = {}
        for arm in prereg["arm"]:
            by_app.setdefault(arm["application"], {})[bool(arm["document_reading"])] = arm["corpus_id"]
        comparisons = {}
        for app, arms in by_app.items():
            legacy, narrow = arms.get(False), arms.get(True)
            comparisons[app] = (
                compare_scores(results[legacy], results[narrow], app)
                if legacy in results and narrow in results
                else {"status": "never_ran"}
            )
        write_json(root / "scores/comparison.json", {
            "status": "not_adopted",
            "reason": "A single paired trial is not an adoption verdict; any application regression rejects adoption.",
            "paired_trial_bars": comparisons,
        }, readonly=True)
    return 1 if failed else 0
