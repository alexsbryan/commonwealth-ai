"""Frozen-input and binary helpers for reader_trial.py."""
import collections
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import tomllib


REPO = pathlib.Path(__file__).resolve().parents[2]
APP = REPO / "research/ontology-apps"
# v3: the same frozen selection, run against the claim-level local-reference
# consistency fix. v1 (20261007-v1) and v2 (20261007-v2) remain the evidence
# of their runs.
PREREG = APP / "reader-trial-prereg-v3.toml"
ROOT = pathlib.Path.home() / ".svrnmesh/bench-corpora/reader-comparison-20261007-v3"
DATA = pathlib.Path.home() / ".svrnmesh"
CHAT_MODEL = "commonwealth/primary"
EMBED_MODEL = "qwen3-embedding-0.6b"
BASE_URL = "http://localhost:9741"
ARM_IDS = (
    "uv-reader-v3-legacy", "uv-reader-v3-narrow",
    "ward-reader-v3-legacy", "ward-reader-v3-narrow",
)


def load_prereg(path=PREREG):
    return tomllib.loads(pathlib.Path(path).read_text(encoding="utf-8"))


def write_json(path, value, *, readonly=False):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)
    if readonly:
        path.chmod(0o444)


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def arm_specs(prereg):
    return [dict(arm) for arm in prereg["arm"]]


def selected_uv(prereg):
    uv = prereg["uv"]
    return list(uv["selected_case_document_ids"]), list(uv["selected_no_case_ids"])


def raw_uv_documents(path):
    rows, by_id = [], {}
    for line in pathlib.Path(path).read_bytes().splitlines(keepends=True):
        if not line.strip():
            continue
        row = json.loads(line)
        doc_id = str(row["id"])
        if doc_id in by_id:
            raise ValueError(f"duplicate UV source id: {doc_id}")
        entry = (doc_id, line, row)
        rows.append(entry)
        by_id[doc_id] = entry
    return rows, by_id


def stage_uv_inputs(root, prereg):
    source = pathlib.Path.home() / ".svrnmesh/bench-corpora/uv-support"
    raw_path, gold_path = source / "raw/documents.jsonl", source / "gold/cases.json"
    rows, by_id = raw_uv_documents(raw_path)
    source_gold = json.loads(gold_path.read_text(encoding="utf-8"))
    selected, no_case = selected_uv(prereg)
    selected_set = set(selected) | set(no_case)
    absent = selected_set - set(by_id)
    if absent:
        raise ValueError(f"pre-registered UV IDs absent from source: {sorted(absent)}")
    absent_id = str(prereg["uv"]["referenced_but_absent"])
    if absent_id in by_id:
        raise ValueError(f"referenced-but-absent issue {absent_id} is present; do not silently reselect")

    cases_of = collections.defaultdict(list)
    for case in source_gold["cases"]:
        for doc_id in case["documents"]:
            cases_of[str(doc_id)].append(case)
    invalid = {
        doc_id: sorted({case.get("fold") for case in cases_of.get(doc_id, [])})
        for doc_id in selected
        if not cases_of.get(doc_id) or any(case.get("fold") != "tune" for case in cases_of[doc_id])
    }
    if invalid:
        raise ValueError(f"selected UV IDs need tune-only gold cases: {invalid}")
    known_none = set(map(str, source_gold["none"]))
    if not set(no_case) <= known_none:
        raise ValueError(f"selected known-no-case IDs absent from source gold: {sorted(set(no_case)-known_none)}")
    threads = {str(by_id[doc_id][2]["thread"]) for doc_id in selected}
    candidates = [
        doc_id for doc_id, _, row in rows
        if str(row.get("thread")) in threads and doc_id in known_none
    ]
    key = lambda doc_id: (
        int(by_id[doc_id][2]["thread"]), str(by_id[doc_id][2].get("created_at", "")), doc_id
    )
    if sorted(candidates, key=key) != no_case:
        raise ValueError(f"deterministic selected-thread no-case list changed: {sorted(candidates, key=key)}")
    ambiguous = sorted(doc_id for doc_id in selected if len(cases_of[doc_id]) > 1)
    if ambiguous != ["1950702048"]:
        raise ValueError(f"selected ambiguity changed: {ambiguous}")

    input_root = pathlib.Path(root) / "inputs/uv"
    (input_root / "raw").mkdir(parents=True)
    (input_root / "gold").mkdir()
    selected_bytes = b"".join(line for doc_id, line, _ in rows if doc_id in selected_set)
    (input_root / "raw/documents.jsonl").write_bytes(selected_bytes)
    case_ids = {case["id"] for doc_id in selected for case in cases_of[doc_id]}
    subset_cases = []
    for case in source_gold["cases"]:
        if case["id"] in case_ids:
            filtered = dict(case)
            filtered["documents"] = [str(doc_id) for doc_id in case["documents"] if str(doc_id) in set(selected)]
            subset_cases.append(filtered)
    states = [
        dict(state) for state in source_gold.get("case_states", [])
        if str(state.get("document")) in set(selected)
    ]
    subset_gold = {
        "repo": source_gold["repo"],
        "fetched_at": source_gold.get("fetched_at"),
        "documents_read": [doc_id for doc_id, _, _ in rows if doc_id in selected_set],
        "cases": subset_cases,
        "case_states": states,
        "none": no_case,
        "uncertain": [],
    }
    write_json(input_root / "gold/cases.json", subset_gold)
    return {
        "application": "uv",
        "source_document_count": len(selected_set),
        "selected_case_document_count": len(selected),
        "selected_no_case_document_count": len(no_case),
        "selected_ambiguous_document_ids": ambiguous,
        "selected_thread_ids": sorted(threads, key=int),
        "selected_document_ids": [doc_id for doc_id, _, _ in rows if doc_id in selected_set],
        "selected_case_ids": sorted(case_ids),
        "source_document_ids": len(by_id),
        "source_documents_sha256": sha256_file(raw_path),
        "source_gold_sha256": sha256_file(gold_path),
        "staged_documents_sha256": sha256_file(input_root / "raw/documents.jsonl"),
        "staged_gold_sha256": sha256_file(input_root / "gold/cases.json"),
        "absent_referenced_id": absent_id,
    }


def stage_ward_inputs(root, prereg):
    source = pathlib.Path.home() / ".svrnmesh/bench-corpora/enron-ward"
    folders = list(prereg["ward"]["folders"])
    input_root = pathlib.Path(root) / "inputs/ward"
    sample_out, gold_out = input_root / "sample", input_root / "gold"
    sample_out.mkdir(parents=True)
    gold_out.mkdir()
    manifest_path = source / "manifest.json"
    shutil.copyfile(manifest_path, input_root / "manifest.json")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    selected_files, gold_files = [], []
    for folder in folders:
        source_dir = source / "sample" / folder
        gold_path = source / "gold" / f"{folder}.json"
        gold = json.loads(gold_path.read_text(encoding="utf-8"))
        names = sorted(path.name for path in source_dir.iterdir() if path.is_file())
        gold_names = set(gold.get("files_read", [])) | set(gold.get("files_with_nothing", []))
        if set(names) != gold_names:
            raise ValueError(f"{folder}: sample/gold file sets differ")
        for name in names:
            out = sample_out / folder / name
            out.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source_dir / name, out)
            selected_files.append(f"{folder}/{name}")
        shutil.copyfile(gold_path, gold_out / gold_path.name)
        gold_files.append(gold_path.name)
    if len(selected_files) != int(prereg["ward"]["expected_source_messages"]):
        raise ValueError(f"Ward source count changed: {len(selected_files)}")
    manifest_paths = {str(entry["path"]) for entry in manifest}
    if set(selected_files) - manifest_paths:
        raise ValueError(f"Ward messages absent from original manifest: {sorted(set(selected_files)-manifest_paths)}")
    return {
        "application": "ward",
        "source_document_count": len(selected_files),
        "folder_counts": {folder: sum(name.startswith(folder + "/") for name in selected_files) for folder in folders},
        "selected_source_files": sorted(selected_files),
        "selected_gold_files": gold_files,
        "manifest_entry_count": len(manifest),
        "original_manifest_sha256": sha256_file(manifest_path),
        "folder_gold_sha256": {name: sha256_file(source / "gold" / name) for name in gold_files},
        "source_file_sha256": {name: sha256_file(sample_out / name) for name in selected_files},
    }


def membership_block(application):
    if application == "uv":
        name, subject = "case_membership", "case"
        desc = "The document concerns this case; this claim asserts membership only and no case state."
    else:
        name, subject = "deal_membership", "deal"
        desc = "The email concerns this deal; this claim asserts membership only and no deal stage."
    return (
        "[[enrichment.ontology.types]]\n"
        f'name = "{name}"\nkind = "claim"\nforce = "assertive"\nsubject = "{subject}"\n'
        f"description = {json.dumps(desc)}\nattributes = []\n\n"
    )


def render_recipe(application, corpus_id, input_path, document_reading):
    subdir = "support" if application == "uv" else "ward"
    text = (APP / subdir / "recipe.toml").read_text(encoding="utf-8")
    text, count = re.subn(r'(?m)^id = "[^"]+"$', f'id = "{corpus_id}"', text, count=1)
    if count != 1:
        raise ValueError(f"cannot locate {application} recipe id")
    if application == "uv":
        pattern = r'(?m)^path = "[^"]*raw/documents\.jsonl"$'
    else:
        pattern = r'(?m)^path = "[^"]*/enron-ward/sample"$'
    text, count = re.subn(pattern, f"path = {json.dumps(str(input_path))}", text, count=1)
    if count != 1:
        raise ValueError(f"cannot locate {application} recipe source path")
    ontology = "[enrichment.ontology]\nversion = 1\n"
    if text.count(ontology) != 1:
        raise ValueError(f"expected one {application} ontology block")
    text = text.replace(ontology, ontology + f"document_reading = {'true' if document_reading else 'false'}\n", 1)
    change = "[enrichment.ontology.change]\n"
    if text.count(change) != 1:
        raise ValueError(f"expected one {application} ontology change block")
    return text.replace(change, membership_block(application) + change, 1)


def git_stamp():
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO, capture_output=True, text=True, check=True).stdout.strip()
    status = subprocess.run(["git", "status", "--short"], cwd=REPO, capture_output=True, text=True, check=True).stdout
    diff_hashes = {}
    for label, path in (("ingest", "ingest"), ("bench", "bench"), ("ontology_apps", "research/ontology-apps")):
        patch = subprocess.run(["git", "diff", "--binary", "--", path], cwd=REPO, capture_output=True, check=True).stdout
        diff_hashes[label] = sha256_bytes(patch)
    others = subprocess.run(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=REPO, capture_output=True, check=True).stdout.decode().split("\0")
    untracked = {
        rel: sha256_file(REPO / rel) for rel in filter(None, others)
        if rel.startswith(("ingest/", "bench/", "shared/", "research/ontology-apps/")) and (REPO / rel).is_file()
    }
    return {
        "head": head,
        "status_sha256": sha256_bytes(status.encode()),
        "changed_paths": [line[3:] for line in status.splitlines() if len(line) > 3],
        "tracked_diff_sha256": diff_hashes,
        "untracked_code_sha256": untracked,
        "reader_trial_sha256": sha256_file(APP / "reader_trial.py"),
        "prereg_sha256": sha256_file(PREREG),
    }


def changed_compile_inputs(prefix):
    tracked = subprocess.run(["git", "diff", "--name-only", "--", prefix], cwd=REPO, capture_output=True, text=True, check=True).stdout.splitlines()
    untracked = subprocess.run(["git", "ls-files", "--others", "--exclude-standard", "-z", "--", prefix], cwd=REPO, capture_output=True, check=True).stdout.decode().split("\0")
    paths = [REPO / rel for rel in [*tracked, *filter(None, untracked)] if (REPO / rel).is_file()]
    return [path for path in paths if path.suffix == ".rs" or path.name in {"Cargo.toml", "build.rs"}]


def freeze_binary(source, destination, changed_prefix):
    if not source.is_file():
        raise FileNotFoundError(f"production binary absent: {source}")
    inputs = changed_compile_inputs(changed_prefix)
    newest = max((path.stat().st_mtime for path in inputs), default=0.0)
    if source.stat().st_mtime < newest:
        names = [str(path.relative_to(REPO)) for path in inputs if path.stat().st_mtime == newest]
        raise RuntimeError(f"binary predates changed compile input(s) {names}: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    destination.chmod(0o555)
    return {
        "source": str(source), "frozen_path": str(destination), "sha256": sha256_file(destination),
        "source_mtime": source.stat().st_mtime, "newest_changed_compile_input_mtime": newest,
        "changed_compile_inputs": [str(path.relative_to(REPO)) for path in inputs],
    }


def freeze_tree(path):
    for item in sorted(path.rglob("*"), reverse=True):
        item.chmod(0o555 if item.is_dir() else 0o444)
    path.chmod(0o555)


def cli_env(root):
    return {
        "SOVEREIGN_INGEST_BIN": str(pathlib.Path(root) / "bin/svrn-ingest"),
        "SOVEREIGN_CLI_BENCH_BIN": str(pathlib.Path(root) / "bin/sovereign-cli-bench"),
    }


def source_ids(application, staged):
    return list(staged[application]["selected_document_ids"] if application == "uv" else staged[application]["selected_source_files"])


def metadata_object(value):
    if isinstance(value, dict):
        return value
    if isinstance(value, bytes):
        value = value.decode("utf-8")
    if isinstance(value, str):
        parsed = json.loads(value or "{}")
        return parsed if isinstance(parsed, dict) else {}
    return {}


def index_snapshot(corpus_id, application, selected, input_root):
    chunks = DATA / "indexes" / corpus_id / "chunks.lance"
    if not chunks.exists():
        return {"status": "could_not_judge", "reason": f"chunks index absent: {chunks}"}
    try:
        import lance
        rows = lance.dataset(str(chunks)).to_table(columns=["metadata"]).to_pylist()
    except Exception as exc:
        return {"status": "could_not_judge", "reason": f"index inspection failed: {type(exc).__name__}: {exc}"}
    selected_set = set(selected)
    by_message = {}
    if application == "ward":
        manifest = json.loads((input_root / "manifest.json").read_text(encoding="utf-8"))
        by_message = {str(mid): str(entry["path"]) for entry in manifest for mid in entry.get("message_ids", [])}
    indexed, unexpected = set(), set()
    for row in rows:
        metadata = metadata_object(row.get("metadata"))
        values = [metadata.get(key) for key in ("message_id", "document_id", "source_doc_id", "id")]
        match = next((str(value) for value in values if value is not None and str(value) in selected_set), None)
        if match is None and application == "ward":
            match = next((by_message[str(value)] for value in values if value is not None and str(value) in by_message), None)
        if match in selected_set:
            indexed.add(match)
        elif any(value is not None for value in values):
            unexpected.add(str(next(value for value in values if value is not None)))
    return {
        "status": "verified", "selected_source_documents": len(selected_set),
        "indexed_selected_documents": len(indexed),
        "not_indexed_source_documents": sorted(selected_set - indexed),
        "unexpected_indexed_document_ids": sorted(unexpected - selected_set),
        "indexed_chunk_count": len(rows),
    }


def validate_fresh_targets(root=ROOT):
    root = pathlib.Path(root)
    if not root.parent.is_dir():
        raise FileNotFoundError(f"bench-corpora parent must already exist: {root.parent}")
    if root.exists():
        raise FileExistsError(f"immutable root already exists: {root}")
    for corpus_id in ARM_IDS:
        for path in (DATA / "indexes" / corpus_id, DATA / "enrichment" / corpus_id):
            if path.exists():
                raise FileExistsError(f"fresh corpus id has existing runtime state: {path}")
