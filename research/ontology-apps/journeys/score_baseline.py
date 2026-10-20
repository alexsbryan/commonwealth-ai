#!/usr/bin/env python3
"""Score a captured issues/news journey run against its frozen source truth."""

from __future__ import annotations

import json
import hashlib
import os
import sys
import tomllib
from collections import Counter
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import urlopen

FIXTURES_RELATIVE = Path("ingest/crates/sovereign-enrichment-build/tests/fixtures/layer")


def repo_root(start):
    """The nearest ancestor of `start` holding the layer fixtures, so the scorer
    runs from wherever it is kept in the repository."""
    for candidate in start.resolve().parents:
        if (candidate / FIXTURES_RELATIVE).is_dir():
            return candidate
    raise SystemExit(f"score_baseline: no ancestor of {start} holds {FIXTURES_RELATIVE}")


ROOT = repo_root(Path(__file__))
FIXTURES = ROOT / FIXTURES_RELATIVE
BASE_URL = os.environ.get("SOVEREIGN_BASE_URL", "http://127.0.0.1:9741").rstrip("/")
MISSING = object()

SEMANTIC_NOTES = {
    "issues": [
        {"query_id": "q2", "text": "The source does not say 'confirmed' literally. The maintainer says the bug is reproducible and 'on our side'; confirmed is a semantic reading within the recipe vocabulary."},
        {"query_id": "q3", "text": "comment-101-3 says 'Please update to 2.4 first', an instruction rather than an explicit state label. Its case_state reading relies on release context."},
    ],
    "news": [
        {"query_id": "q2", "text": "The workers-treated sentence is verbatim in both reports. Treating the two mentions as one happening is supported by the repeated passage, but co-reference is inferred."},
    ],
}


def strict_json(text):
    def unique_pairs(pairs):
        out = {}
        for key, value in pairs:
            if key in out:
                raise ValueError(f"duplicate JSON key {key!r}")
            out[key] = value
        return out

    return json.loads(text, object_pairs_hook=unique_pairs)


def item(code, message, verdict="FAILED", **extra):
    return {"code": code, "message": message, "verdict": verdict, **extra}


def toml(path):
    with path.open("rb") as stream:
        return tomllib.load(stream)


def read_fixture(name):
    if name not in {"issues", "news"}:
        raise ValueError("fixture must be 'issues' or 'news'")
    folder = FIXTURES / name
    expected = toml(folder / "expected.toml")
    recipe = toml(folder / "recipe.toml")
    source = expected.get("source")
    if not isinstance(source, str):
        raise ValueError("expected.toml has no string source")
    documents = {}
    problems = []
    for line_no, line in enumerate((folder / source).read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        try:
            doc = strict_json(line)
        except (json.JSONDecodeError, ValueError) as exc:
            problems.append(item("source-json-invalid", f"{source}:{line_no}: {exc}"))
            continue
        if not isinstance(doc, dict) or not isinstance(doc.get("id"), str) or doc["id"] in documents:
            problems.append(item("source-id-invalid", f"{source}:{line_no}: document id missing or duplicated"))
            continue
        documents[doc["id"]] = doc
    queries = {}
    for query in expected.get("query", []):
        qid = query.get("id")
        filename = query.get("file")
        if not isinstance(qid, str) or not isinstance(filename, str):
            problems.append(item("query-definition-invalid", f"query definition lacks string id/file: {query!r}"))
            continue
        try:
            queries[qid] = strict_json((folder / filename).read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError, ValueError) as exc:
            problems.append(item("query-input-invalid", f"{qid}: {exc}", query_id=qid))
    return folder, expected, recipe, documents, queries, problems


def recipe_types(recipe):
    types = recipe.get("enrichment", {}).get("ontology", {}).get("types", [])
    return {t["name"]: t for t in types if isinstance(t, dict) and isinstance(t.get("name"), str)}


def bind_type(expected, bind):
    for target in expected.get("lookup", {}).get("targets", []):
        if target.get("bind") == bind:
            return target.get("type")
    for question in expected.get("query", []):
        for row in question.get("rows", []):
            if row.get("bind") == bind:
                return row.get("type")
    return None


def source_quote(docs, source_id, text, label, qid=None):
    doc = docs.get(source_id) if isinstance(source_id, str) else None
    if doc is None:
        return item("truth-source-unknown", f"{label}: source document {source_id!r} is absent", query_id=qid)
    body = doc.get("body")
    if not isinstance(body, str) or not isinstance(text, str) or not text:
        return item("truth-quote-invalid", f"{label}: source body or quote is missing", query_id=qid)
    if text not in body:
        return item("truth-quote-not-in-source", f"{label}: exact quote is absent from {source_id!r}: {text!r}", query_id=qid)
    return None


def source_metadata(docs, source_id, field, value, label, qid=None):
    doc = docs.get(source_id) if isinstance(source_id, str) else None
    if doc is None or not isinstance(field, str) or field not in doc:
        return item("truth-metadata-source-invalid", f"{label}: source {source_id!r} has no field {field!r}", query_id=qid)
    if doc[field] != value:
        return item("truth-metadata-value-wrong", f"{label}: {source_id}.{field} is {doc[field]!r}, expected {value!r}", query_id=qid)
    return None


def is_optional(row):
    """A frozen row the source leaves undetermined (`optional = true`): scored
    when a query row binds to it, never omitted when absent, never extra."""
    return isinstance(row, dict) and row.get("optional") is True


def matched_bounds(spec):
    """`expected_matched` as (min, max) inclusive: an integer n is (n, n), a
    two-integer list [min, max] is itself; anything else is None."""
    if type(spec) is int:
        return spec, spec
    if isinstance(spec, list) and len(spec) == 2 and all(type(x) is int for x in spec) and spec[0] <= spec[1]:
        return spec[0], spec[1]
    return None


def audit_truth(name, expected, recipe, docs, queries, initial):
    types = recipe_types(recipe)
    findings = list(initial)
    invalid_keys = set()
    for question in expected.get("query", []):
        qid = question.get("id")
        if not isinstance(qid, str):
            continue
        rows = question.get("rows", [])
        matched = question.get("expected_matched", MISSING)
        required = sum(not is_optional(row) for row in rows)
        if matched_bounds(matched) != (required, len(rows)) or (type(matched) is int and required != len(rows)):
            want = required if required == len(rows) else [required, len(rows)]
            findings.append(item("truth-count-inconsistent", f"{qid}: expected_matched must be {want!r}: {required} required and {len(rows) - required} optional frozen rows", query_id=qid))
        query = queries.get(qid)
        if not isinstance(query, dict) or not isinstance(query.get("target_type"), str):
            findings.append(item("query-target-type-missing", f"{qid}: query file has no target_type", query_id=qid))
        for row in rows:
            bind, row_type = row.get("bind", "<unbound>"), row.get("type")
            declaration = types.get(row_type)
            if declaration is None:
                findings.append(item("truth-type-undeclared", f"{qid}/{bind}: {row_type!r} is not declared", query_id=qid))
                continue
            if isinstance(query, dict) and query.get("target_type") != row_type:
                findings.append(item("query-target-type-mismatch", f"{qid}/{bind}: query targets {query.get('target_type')!r}, frozen row is {row_type!r}", query_id=qid))
            if isinstance(query, dict) and query.get("target_type") != row_type:
                findings.append(item("query-target-type-mismatch", f"{qid}/{bind}: query targets {query.get('target_type')!r}, row is {row_type!r}", query_id=qid))
            attrs = {a.get("name"): a for a in declaration.get("attributes", []) if isinstance(a, dict)}
            for attr, value in row.get("attributes", {}).items():
                spec = attrs.get(attr)
                if spec is None:
                    findings.append(item("truth-attribute-undeclared", f"{qid}/{bind}: {attr!r} is not declared", query_id=qid))
                elif isinstance(spec.get("values"), list) and value not in spec["values"]:
                    findings.append(item("truth-value-undeclared", f"{qid}/{bind}: {attr}={value!r} is outside recipe vocabulary", query_id=qid))
            identity = row.get("id_bind_from")
            if not isinstance(identity, dict):
                findings.append(item("truth-row-binding-missing", f"{qid}/{bind}: id_bind_from is missing", query_id=qid))
            else:
                problem = source_quote(docs, identity.get("source_doc_id"), identity.get("quote"), f"{qid}/{bind} id_bind_from", qid)
                if problem:
                    findings.append(problem)
            for evidence in row.get("evidence", []) + row.get("attribute_evidence", []):
                problem = source_quote(docs, evidence.get("source_doc_id"), evidence.get("quote"), f"{qid}/{bind} evidence", qid)
                if problem:
                    findings.append(problem)
            for ref in row.get("refs", []):
                target_type = bind_type(expected, ref.get("target_bind"))
                declared_target = ref.get("target_type") or target_type
                special_subject = ref.get("attribute") == "subject" and declaration.get("subject") == declared_target
                ref_spec = attrs.get(ref.get("attribute"), {})
                ordinary_ref = ref_spec.get("type") == "ref" and ref_spec.get("of") == declared_target
                if not special_subject and not ordinary_ref:
                    findings.append(item("truth-ref-undeclared", f"{qid}/{bind}: {ref.get('attribute')!r} does not point to {declared_target!r} in recipe.toml", query_id=qid))
                if target_type is None or (ref.get("target_type") is not None and ref["target_type"] != target_type):
                    findings.append(item("truth-ref-target-type-mismatch", f"{qid}/{bind}: frozen ref target type conflicts with bind {ref.get('target_bind')!r}", query_id=qid))
                if "key" in ref:
                    target_decl = types.get(target_type, {})
                    identity_fields = target_decl.get("identity", [])
                    identity_fields = [identity_fields] if isinstance(identity_fields, str) else identity_fields
                    target = next((t for t in expected.get("lookup", {}).get("targets", []) if t.get("bind") == ref.get("target_bind")), None)
                    values = target.get("attributes", {}) if target else {}
                    key = (qid, bind, ref.get("attribute"), "key")
                    if not identity_fields or any(field not in values for field in identity_fields):
                        invalid_keys.add(key)
                        findings.append(item("invalid-frozen-key-requirement", f"{qid}/{bind} ref {ref.get('attribute')!r}: key {ref['key']!r} is not derivable from declared identity fields {identity_fields!r}; RecordView.key reads identity, not identity_necessary", "COULD-NOT-JUDGE", query_id=qid))
                    elif ", ".join(str(values[field]) for field in identity_fields) != ref["key"]:
                        invalid_keys.add(key)
                        findings.append(item("invalid-frozen-key-requirement", f"{qid}/{bind} ref {ref.get('attribute')!r}: key {ref['key']!r} disagrees with identity values", "COULD-NOT-JUDGE", query_id=qid))
                if isinstance(ref.get("source"), dict) and isinstance(identity, dict):
                    source = ref["source"]
                    problem = source_metadata(docs, identity.get("source_doc_id"), source.get("field"), source.get("value"), f"{qid}/{bind} ref source", qid)
                    if problem:
                        findings.append(problem)

    for target in expected.get("lookup", {}).get("targets", []):
        bind = target.get("bind", "<unbound>")
        declaration = types.get(target.get("type"))
        if declaration is None:
            findings.append(item("truth-target-type-undeclared", f"lookup target {bind}: type {target.get('type')!r} is not declared"))
            continue
        attrs = {a.get("name"): a for a in declaration.get("attributes", []) if isinstance(a, dict)}
        for attr, value in target.get("attributes", {}).items():
            spec = attrs.get(attr)
            if spec is None or (isinstance(spec.get("values"), list) and value not in spec["values"]):
                findings.append(item("truth-target-attribute-invalid", f"lookup target {bind}: {attr}={value!r} is not declared by recipe.toml"))
        for evidence in target.get("evidence", []):
            problem = source_quote(docs, evidence.get("source_doc_id"), evidence.get("quote"), f"lookup target {bind} evidence")
            if problem:
                findings.append(problem)
        identity = target.get("identity_source")
        if isinstance(identity, dict):
            problem = source_metadata(docs, identity.get("source_doc_id"), identity.get("metadata_field"), identity.get("value"), f"lookup target {bind} identity")
            if problem:
                findings.append(problem)
            source = declaration.get("source", {})
            if identity.get("metadata_field") not in source.get("metadata", []):
                findings.append(item("truth-identity-source-not-declared", f"lookup target {bind}: metadata field {identity.get('metadata_field')!r} is not a recipe source"))

    for unknown in expected.get("unknown", []):
        problem = source_quote(docs, unknown.get("source_doc_id"), unknown.get("quote"), f"negative control {unknown.get('record_bind', '<unbound>')}")
        if problem:
            findings.append(problem)
    return {
        "status": "invalid" if findings or invalid_keys else "valid",
        "findings": findings,
        "semantic_notes": SEMANTIC_NOTES[name],
        "invalid_requirements": [list(key) for key in sorted(invalid_keys)],
    }


def read_query_output(path):
    if not path.exists():
        return {"state": "NEVER-RAN", "output": None, "error": f"{path.name} is absent", "path": path.name}
    text = path.read_text(encoding="utf-8")
    if not text.strip():
        return {"state": "COULD-NOT-JUDGE", "output": None, "error": f"{path.name} is empty", "path": path.name}
    try:
        output = strict_json(text)
    except (json.JSONDecodeError, ValueError) as exc:
        return {"state": "FAILED", "output": None, "error": f"{path.name} is invalid JSON: {exc}", "path": path.name}
    if not isinstance(output, dict):
        return {"state": "FAILED", "output": None, "error": f"{path.name} JSON root is not an object", "path": path.name}
    return {"state": "OUTPUT", "output": output, "error": None, "path": path.name}


def parse_lookup(path, corpus):
    result = {"state": "NEVER-RAN", "payload": None, "by_id": {}, "complete": False, "truncated": False, "tool_ids": [], "findings": []}
    if not path.exists():
        return result
    lines = [line for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    if not lines:
        result["state"] = "COULD-NOT-JUDGE"
        result["findings"].append(item("atoms-lookup-empty", "atoms_lookup.jsonl is empty", "COULD-NOT-JUDGE"))
        return result
    replies = {}
    for line_no, line in enumerate(lines, 1):
        try:
            message = strict_json(line)
        except (json.JSONDecodeError, ValueError) as exc:
            result["findings"].append(item("mcp-jsonl-invalid", f"line {line_no}: {exc}"))
            result["state"] = "FAILED"
            continue
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0" or type(message.get("id")) is not int:
            result["findings"].append(item("mcp-envelope-invalid", f"line {line_no}: expected JSON-RPC 2.0 response with integer id"))
            result["state"] = "FAILED"
            continue
        if message["id"] in replies:
            result["findings"].append(item("mcp-id-duplicate", f"duplicate response id {message['id']}"))
            result["state"] = "FAILED"
        replies[message["id"]] = message
    result["tool_ids"] = sorted(replies)
    for rpc_id, label in ((1, "initialize"), (2, "tools/list")):
        if rpc_id not in replies or not isinstance(replies[rpc_id].get("result"), dict):
            result["findings"].append(item("mcp-response-missing", f"response id {rpc_id} ({label}) is absent or has no object result"))
            result["state"] = "FAILED"
    if 2 in replies and isinstance(replies[2].get("result"), dict):
        tools = replies[2]["result"].get("tools", MISSING)
        names = [tool.get("name") for tool in tools if isinstance(tool, dict)] if isinstance(tools, list) else []
        if "atoms_lookup" not in names:
            result["findings"].append(item("atoms-lookup-not-listed", "tools/list did not advertise atoms_lookup"))
            result["state"] = "FAILED"
    if 3 not in replies:
        result["findings"].append(item("atoms-lookup-call-missing", "response id 3 (tools/call) is absent", "COULD-NOT-JUDGE"))
        if result["state"] != "FAILED":
            result["state"] = "COULD-NOT-JUDGE"
        return result
    call = replies[3]
    if "error" in call:
        result["findings"].append(item("atoms-lookup-rpc-error", f"tools/call returned {call['error']!r}", "COULD-NOT-JUDGE"))
        if result["state"] != "FAILED":
            result["state"] = "COULD-NOT-JUDGE"
        return result
    call_result = call.get("result", MISSING)
    if not isinstance(call_result, dict) or call_result.get("isError", MISSING) is not False:
        result["findings"].append(item("atoms-lookup-call-failed", "tools/call lacks result/isError=false", "COULD-NOT-JUDGE"))
        if result["state"] != "FAILED":
            result["state"] = "COULD-NOT-JUDGE"
        return result
    payload = call_result.get("structuredContent", MISSING)
    if not isinstance(payload, dict):
        result["findings"].append(item("atoms-lookup-structured-missing", "tools/call has no structuredContent object", "COULD-NOT-JUDGE"))
        if result["state"] != "FAILED":
            result["state"] = "COULD-NOT-JUDGE"
        return result
    result["payload"] = payload
    required = ("corpus", "total_atoms", "matching", "offset", "next_offset", "refs_resolved", "atoms")
    absent = [field for field in required if field not in payload]
    if absent:
        result["findings"].append(item("atoms-lookup-field-missing", f"structuredContent fields absent: {absent!r}"))
        result["state"] = "FAILED"
    if payload.get("corpus", MISSING) != corpus:
        result["findings"].append(item("atoms-lookup-corpus-wrong", f"corpus is {payload.get('corpus', '<missing>')!r}, expected {corpus!r}"))
        result["state"] = "FAILED"
    total, matched, offset, atoms = (payload.get(field, MISSING) for field in ("total_atoms", "matching", "offset", "atoms"))
    if type(total) is not int or type(matched) is not int or type(offset) is not int or not isinstance(atoms, list):
        result["findings"].append(item("atoms-lookup-field-type", "total_atoms/matching/offset must be integers and atoms an array"))
        result["state"] = "FAILED"
    else:
        next_offset = payload.get("next_offset", MISSING)
        result["truncated"] = next_offset is not None or len(atoms) != matched or matched != total or offset != 0
        result["complete"] = not result["truncated"]
        if result["truncated"]:
            result["findings"].append(item("atoms-lookup-truncated", f"returned {len(atoms)} of {matched} rows, total {total}; absent ids cannot be judged", "COULD-NOT-JUDGE"))
    if payload.get("refs_resolved", MISSING) is not True:
        result["findings"].append(item("atoms-lookup-refs-unresolved", "refs_resolved is absent or false"))
        result["state"] = "FAILED"
    if isinstance(atoms, list):
        for index, atom in enumerate(atoms):
            if not isinstance(atom, dict) or not isinstance(atom.get("id"), str):
                result["findings"].append(item("atoms-lookup-row-invalid", f"atoms[{index}] has no string id"))
                result["state"] = "FAILED"
                continue
            missing = [field for field in ("type", "attributes", "refs", "evidence_chunks") if field not in atom]
            if missing:
                result["findings"].append(item("atoms-lookup-row-fields-missing", f"atoms[{index}] lacks {missing!r}"))
                result["state"] = "FAILED"
            if atom["id"] in result["by_id"]:
                result["findings"].append(item("atoms-lookup-id-duplicate", f"duplicate atom id {atom['id']!r}"))
                result["state"] = "FAILED"
            result["by_id"][atom["id"]] = atom
    if result["state"] not in {"FAILED", "COULD-NOT-JUDGE"}:
        result["state"] = "complete" if result["complete"] else "partial"
    return result


def chunk_ids_from(query_runs, lookup):
    found = set()
    for run in query_runs.values():
        output = run.get("output")
        rows = output.get("rows") if isinstance(output, dict) else None
        if isinstance(rows, list):
            for row in rows:
                if isinstance(row, dict):
                    found.update(x for x in row.get("evidence", []) if isinstance(x, str)) if isinstance(row.get("evidence"), list) else None
                    record = row.get("record")
                    if isinstance(record, dict):
                        if isinstance(record.get("evidence_chunks"), list):
                            found.update(x for x in record["evidence_chunks"] if isinstance(x, str))
    atoms = lookup.get("payload", {}).get("atoms") if isinstance(lookup.get("payload"), dict) else None
    if isinstance(atoms, list):
        for atom in atoms:
            if isinstance(atom, dict) and isinstance(atom.get("evidence_chunks"), list):
                found.update(x for x in atom["evidence_chunks"] if isinstance(x, str))
    return sorted(found)


def inspect_chunks(corpus, ids):
    records, findings, blocked = {}, [], None
    for chunk_id in ids:
        if blocked:
            records[chunk_id] = {"state": "COULD-NOT-JUDGE", "reason": blocked}
            continue
        url = f"{BASE_URL}/v1/corpora/{quote(corpus, safe='')}/chunks/{quote(chunk_id, safe='')}?radius=0"
        try:
            with urlopen(url, timeout=3) as response:
                body = response.read().decode("utf-8")
            value = strict_json(body)
        except HTTPError as exc:
            if exc.code in (400, 404):
                records[chunk_id] = {"state": "missing", "reason": f"HTTP {exc.code}"}
            else:
                blocked = f"HTTP {exc.code}"
                records[chunk_id] = {"state": "COULD-NOT-JUDGE", "reason": blocked}
        except (URLError, TimeoutError, OSError) as exc:
            blocked = f"{type(exc).__name__}: {exc}"
            records[chunk_id] = {"state": "COULD-NOT-JUDGE", "reason": blocked}
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
            records[chunk_id] = {"state": "FAILED", "reason": f"invalid JSON: {exc}"}
        else:
            center = value.get("center", MISSING) if isinstance(value, dict) else MISSING
            if not isinstance(center, dict) or "chunk_id" not in center or center.get("corpus_id") != corpus or not isinstance(center.get("content"), str):
                records[chunk_id] = {"state": "FAILED", "reason": "public reading response lacks the requested corpus chunk shape"}
            else:
                records[chunk_id] = {"state": "ok", "center": center}
    for chunk_id, record in records.items():
        if record["state"] != "ok":
            verdict = "COULD-NOT-JUDGE" if record["state"] == "COULD-NOT-JUDGE" else "FAILED"
            findings.append(item("chunk-inspection-" + record["state"].lower(), f"chunk {chunk_id!r}: {record['reason']}", verdict))
    return records, findings


def quote_hits(ids, source_id, text, chunks):
    matches, wrong, unknown = [], [], []
    for chunk_id in ids:
        record = chunks.get(chunk_id)
        if record is None or record["state"] == "COULD-NOT-JUDGE":
            unknown.append(chunk_id)
        elif record["state"] == "ok":
            center = record["center"]
            if text in center["content"]:
                (matches if center.get("source_doc_id") == source_id else wrong).append(chunk_id)
    return matches, wrong, unknown


def add(report, code, message, verdict="FAILED", bucket="bad"):
    report["findings"].append(item(code, message, verdict, query_id=report["id"]))
    report["_failed"] |= verdict == "FAILED"
    report["_cnd"] |= verdict == "COULD-NOT-JUDGE"
    metric = {
        "bad": "bad_or_missing_values",
        "citation": "citation_failures",
        "lookup": "lookup_disagreements",
        "surface": "query_surface_failures",
    }.get(bucket)
    if metric:
        report[metric] += 1


def check_attrs(actual, expected, report, label):
    if not isinstance(actual, dict):
        add(report, "attributes-missing", f"{label}: attributes is not an object")
        return
    for key, value in expected.items():
        if key not in actual:
            add(report, "attribute-missing", f"{label}: {key!r} is absent")
        elif actual[key] != value:
            add(report, "attribute-wrong", f"{label}: {key}={actual[key]!r}, expected {value!r}")


def check_evidence(actual_ids, expected, chunks, report, label):
    if not isinstance(actual_ids, list):
        add(report, "evidence-missing", f"{label}: evidence_chunks is missing or not an array", bucket="citation")
        return
    ids = [value for value in actual_ids if isinstance(value, str)]
    for evidence in expected:
        matches, wrong, unknown = quote_hits(ids, evidence.get("source_doc_id"), evidence.get("quote"), chunks)
        if matches:
            continue
        if unknown:
            add(report, "evidence-could-not-judge", f"{label}: cannot inspect chunks {unknown!r}", "COULD-NOT-JUDGE", "citation")
        elif wrong:
            add(report, "evidence-wrong-source", f"{label}: exact quote appears under wrong source in chunks {wrong!r}", bucket="citation")
        else:
            add(report, "evidence-quote-missing", f"{label}: no cited chunk from {evidence.get('source_doc_id')!r} contains the frozen quote", bucket="citation")


def validate_row(qid, truth, row, chunks, report, invalid_keys, expected):
    bind = truth.get("bind", "<unbound>")
    label = f"{qid}/{bind}"
    if not isinstance(row, dict):
        add(report, "query-row-invalid", f"{label}: output row is not an object")
        return
    record = row.get("record", MISSING)
    atom_id = row.get("atom_id", MISSING)
    if not isinstance(atom_id, str) or not atom_id:
        add(report, "atom-id-missing", f"{label}: atom_id is missing or not a non-empty string")
    if not isinstance(record, dict):
        add(report, "record-missing", f"{label}: typed row has no RecordView object")
        return
    if record.get("id", MISSING) != atom_id:
        add(report, "record-id-mismatch", f"{label}: atom_id and RecordView id disagree or one is missing")
    if record.get("type", MISSING) != truth.get("type"):
        add(report, "type-wrong", f"{label}: RecordView type {record.get('type', '<missing>')!r}, expected {truth.get('type')!r}")
    check_attrs(row.get("attributes", MISSING), truth.get("attributes", {}), report, f"{label} typed row")
    check_attrs(record.get("attributes", MISSING), truth.get("attributes", {}), report, f"{label} RecordView")
    own_evidence = record.get("evidence_chunks", MISSING)
    if not isinstance(own_evidence, list):
        add(report, "record-evidence-missing", f"{label}: evidence_chunks missing or not an array", bucket="citation")
    else:
        typed_evidence = row.get("evidence", MISSING)
        if not isinstance(typed_evidence, list):
            add(report, "typed-evidence-missing", f"{label}: typed row evidence missing or not an array", bucket="citation")
        else:
            for chunk_id in (x for x in own_evidence if isinstance(x, str)):
                if chunk_id not in typed_evidence:
                    add(report, "typed-evidence-drops-record", f"{label}: typed row omits RecordView evidence chunk {chunk_id!r}", bucket="citation")
    check_evidence(own_evidence, truth.get("evidence", []), chunks, report, f"{label} evidence")
    for evidence in truth.get("attribute_evidence", []):
        attrs = record.get("attributes", {})
        if not isinstance(attrs, dict) or evidence.get("attribute") not in attrs:
            add(report, "attribute-evidence-value-missing", f"{label}: attribute {evidence.get('attribute')!r} is absent", bucket="citation")
        check_evidence(own_evidence, [evidence], chunks, report, f"{label} attribute {evidence.get('attribute')!r}")
    refs = ref_rows(record)
    if refs is None:
        add(report, "refs-missing", f"{label}: RecordView refs missing or not an array")
        return
    for ref in refs:
        if not isinstance(ref, dict):
            add(report, "ref-row-invalid", f"{label}: a ref entry is not an object")
    expected_refs = truth.get("refs", [])
    expected_names = {ref.get("attribute") for ref in expected_refs}
    actual_names = {ref.get("attribute") for ref in refs if isinstance(ref, dict)}
    for extra in actual_names - expected_names:
        add(report, "unexpected-ref", f"{label}: unexpected reference attribute {extra!r}")
    for ref in expected_refs:
        attr = ref.get("attribute")
        found = [value for value in refs if isinstance(value, dict) and value.get("attribute") == attr]
        if len(found) != 1:
            add(report, "ref-missing-or-ambiguous", f"{label}: expected one {attr!r} ref, found {len(found)}")
            continue
        actual = found[0]
        if not isinstance(actual.get("id"), str):
            add(report, "ref-id-missing", f"{label}: ref {attr!r} has no string id")
        target_type = ref.get("target_type") or bind_type(expected, ref.get("target_bind"))
        if actual.get("type", MISSING) != target_type:
            add(report, "ref-type-wrong", f"{label}: ref {attr!r} type {actual.get('type', '<missing>')!r}, expected {target_type!r}")
        key = (qid, bind, attr, "key")
        if "key" in ref and key in invalid_keys:
            add(report, "invalid-frozen-key-not-judged", f"{label}: frozen key {ref['key']!r} is not declared by the recipe", "COULD-NOT-JUDGE", "truth")
        elif "key" in ref:
            if actual.get("key", MISSING) != ref["key"]:
                add(report, "ref-key-wrong", f"{label}: ref {attr!r} key {actual.get('key', '<missing>')!r}, expected {ref['key']!r}")


def by_row_text(rows, candidates, quote):
    """Of several rows citing the frozen chunk, the one whose own text holds
    the id_bind_from quote; all of them when none or several do (ambiguous)."""
    holding = []
    for index in candidates:
        row = rows[index] if isinstance(rows[index], dict) else {}
        record = row.get("record") if isinstance(row.get("record"), dict) else {}
        texts = [t for t in (row.get("text"), record.get("text")) if isinstance(t, str)]
        if isinstance(quote, str) and quote and any(quote in t for t in texts):
            holding.append(index)
    return holding if len(holding) == 1 else candidates


def optional_unbound(expected, contexts, spec):
    """True when an id_from spec names an optional row no query row bound."""
    if not isinstance(spec, dict):
        return False
    question = next((q for q in expected.get("query", []) if q.get("id") == spec.get("query_id")), None)
    truth = next((r for r in (question or {}).get("rows", []) if r.get("bind") == spec.get("row_bind")), None)
    context = contexts.get(spec.get("query_id"), {})
    return is_optional(truth) and spec.get("row_bind") not in context.get("bound_rows", {})


def ref_rows(record):
    return record.get("refs") if isinstance(record, dict) and isinstance(record.get("refs"), list) else None


def lookup_compare(row, lookup, report, label):
    if not isinstance(row, dict) or not isinstance(row.get("atom_id"), str) or not isinstance(row.get("record"), dict):
        return
    atom_id, record = row["atom_id"], row["record"]
    atom = lookup["by_id"].get(atom_id)
    if atom is None:
        if lookup["complete"]:
            add(report, "lookup-row-absent", f"{label}: query atom {atom_id!r} is absent from complete atoms_lookup", bucket="lookup")
        else:
            add(report, "lookup-row-could-not-judge", f"{label}: atoms_lookup did not return {atom_id!r} and is incomplete", "COULD-NOT-JUDGE", "lookup")
        return
    for field in ("id", "type", "attributes", "refs", "evidence_chunks"):
        if field not in record or field not in atom:
            add(report, "lookup-field-missing", f"{label}: query or atoms_lookup record lacks {field!r}", bucket="lookup")
        elif record[field] != atom[field]:
            add(report, "lookup-disagreement", f"{label}: RecordView {field} differs between atlas-query and atoms_lookup", bucket="lookup")


def score(expected, query_inputs, query_runs, lookup, chunks, invalid_keys):
    questions, contexts, bindings = [], {}, {}
    for question in expected.get("query", []):
        qid = question["id"]
        report = {
            "id": qid, "question": question.get("question"), "kind": question.get("kind"), "query": query_inputs.get(qid),
            "expected_matched": question.get("expected_matched") if matched_bounds(question.get("expected_matched")) else None,
            "actual_matched": None, "expected_rows": len(question.get("rows", [])),
            "optional_rows": sum(is_optional(row) for row in question.get("rows", [])),
            "expected_rows_present": None, "omitted_expected_rows": None, "extra_rows": None,
            "ambiguous_bindings": 0, "bad_or_missing_values": 0, "citation_failures": 0, "lookup_disagreements": 0,
            "query_surface_failures": 0,
            "split_ref_ids": 0, "falsemerged_ref_ids": 0, "split_row_ids": 0, "falsemerged_row_ids": 0,
            "matched_count_mismatches": 0, "findings": [], "_failed": False, "_cnd": False,
        }
        run, expected_rows = query_runs.get(qid, {}), question.get("rows", [])
        output, state = run.get("output"), run.get("state", "NEVER-RAN")
        rows = output.get("rows", MISSING) if isinstance(output, dict) else MISSING
        matched = output.get("matched", MISSING) if isinstance(output, dict) else MISSING
        if state == "NEVER-RAN":
            add(report, "never-ran", f"{qid}.out is absent", "NEVER-RAN")
        elif state == "COULD-NOT-JUDGE":
            add(report, "query-output-missing", run.get("error", "stdout unavailable"), "COULD-NOT-JUDGE")
        elif state == "FAILED":
            add(report, "query-json-invalid", run.get("error", "stdout is not valid JSON"))
        else:
            hit = output.get("hit", MISSING)
            if type(hit) is not bool:
                add(report, "hit-missing", f"{qid}: native JSON has no boolean hit field")
            elif not hit:
                headline = output.get("headline")
                if isinstance(headline, str) and (
                    "names no ref attribute of a declared type" in headline
                    or "neither a declared relation type nor a ref" in headline
                ):
                    add(report, "typed-query-surface-refused", f"{qid}: {headline}", bucket="surface")
                else:
                    add(report, "query-miss", f"{qid}: hit=false ({headline or 'no refusal reason'})")
                rows = None
                report["expected_rows_present"] = 0
                report["omitted_expected_rows"] = None
                report["extra_rows"] = None
            else:
                if type(matched) is not int:
                    add(report, "matched-missing", f"{qid}: native JSON has no integer matched field")
                else:
                    report["actual_matched"] = matched
                    bounds = matched_bounds(question.get("expected_matched"))
                    if bounds is None or not bounds[0] <= matched <= bounds[1]:
                        report["matched_count_mismatches"] += 1
                        add(report, "matched-wrong", f"{qid}: matched={matched}, expected {question.get('expected_matched')!r}", bucket="count")
                if rows is MISSING or not isinstance(rows, list):
                    add(report, "rows-missing", f"{qid}: native JSON rows field is missing or not an array")
                    rows = None
                elif type(matched) is int and len(rows) != matched:
                    add(report, "row-length-mismatch", f"{qid}: rows has {len(rows)} entries but matched={matched}", bucket="count")

        candidate_map, unresolved_map, bound_rows = {}, {}, {}
        if isinstance(rows, list):
            taken = set()
            # Required rows bind first; an optional row never takes a row a
            # required one bound (one claim citing a shared chunk is the
            # required row's, not a false merge of both).
            for truth in sorted(expected_rows, key=is_optional):
                bind = truth.get("bind", "<unbound>")
                candidates, unresolved, wrong_source = [], False, False
                ident = truth.get("id_bind_from", {})
                for index, row in enumerate(rows):
                    record = row.get("record") if isinstance(row, dict) else None
                    ids = record.get("evidence_chunks", MISSING) if isinstance(record, dict) else MISSING
                    if not isinstance(ids, list):
                        continue
                    hits, wrong, unknown = quote_hits([x for x in ids if isinstance(x, str)], ident.get("source_doc_id"), ident.get("quote"), chunks)
                    if hits:
                        candidates.append(index)
                    else:
                        wrong_source |= bool(wrong)
                    if not hits and unknown:
                        unresolved = True
                if is_optional(truth):
                    candidates = [index for index in candidates if index not in taken]
                if len(candidates) > 1:
                    candidates = by_row_text(rows, candidates, ident.get("quote"))
                candidate_map[bind], unresolved_map[bind] = candidates, unresolved
                if wrong_source and not candidates and not unresolved:
                    add(report, "row-evidence-wrong-source", f"{qid}/{bind}: exact quote occurs only under a different source_doc_id", bucket="citation")
                if len(candidates) == 1:
                    taken.add(candidates[0])
                    row = rows[candidates[0]]
                    bound_rows[bind] = row
                    atom_id = row.get("atom_id") if isinstance(row, dict) else None
                    if isinstance(atom_id, str):
                        bindings[bind] = atom_id
                        validate_row(qid, truth, row, chunks, report, invalid_keys, expected)
                elif len(candidates) > 1:
                    report["ambiguous_bindings"] += 1
                    add(report, "row-binding-ambiguous", f"{qid}/{bind}: source/quote matches {len(candidates)} output rows")
                elif not unresolved and not is_optional(truth):
                    add(report, "expected-row-omitted", f"{qid}/{bind}: no row cites the frozen source and exact quote", bucket="count")
            present = sum(bool(candidate_map.get(row.get("bind"))) for row in expected_rows)
            unresolved = any(unresolved_map.values())
            covered = {i for ids in candidate_map.values() for i in ids}
            report["expected_rows_present"] = present
            report["omitted_expected_rows"] = None if unresolved else sum(not candidate_map.get(row.get("bind")) for row in expected_rows if not is_optional(row))
            report["extra_rows"] = None if unresolved else max(len(rows) - len(covered), max(0, len(rows) - len(expected_rows)))
            if unresolved:
                add(report, "row-binding-unavailable", f"{qid}: chunk inspection was unavailable for a possible binding", "COULD-NOT-JUDGE", "count")
            if report["extra_rows"]:
                add(report, "extra-rows", f"{qid}: {report['extra_rows']} output row(s) are outside the frozen partition", bucket="count")
            use_count = Counter(index for values in candidate_map.values() if len(values) == 1 for index in values)
            duplicated = [index for index, count in use_count.items() if count > 1]
            if duplicated:
                report["falsemerged_row_ids"] += len(duplicated)
                add(report, "row-id-false-merge", f"{qid}: multiple frozen bindings resolve to output row(s) {duplicated!r}")
            for index, row in enumerate(rows):
                lookup_compare(row, lookup, report, f"{qid}/rows[{index}]")
        contexts[qid] = {"question": question, "rows": rows, "bound_rows": bound_rows, "candidates": candidate_map}
        questions.append(report)

    reports = {report["id"]: report for report in questions}
    same_id_targets = set()
    target_rows = []
    for target in expected.get("lookup", {}).get("targets", []):
        specs = target.get("id_from", [])
        specs = [specs] if isinstance(specs, dict) else specs
        observed, owner = [], reports.get(specs[0].get("query_id")) if specs and isinstance(specs[0], dict) else None
        for spec in specs if isinstance(specs, list) else []:
            context = contexts.get(spec.get("query_id")) if isinstance(spec, dict) else None
            row = context.get("bound_rows", {}).get(spec.get("row_bind")) if context else None
            refs = ref_rows(row.get("record")) if isinstance(row, dict) else None
            matches = [r for r in refs if isinstance(r, dict) and r.get("attribute") == spec.get("ref_attribute")] if refs else []
            if len(matches) == 1 and isinstance(matches[0].get("id"), str):
                observed.append(matches[0]["id"])
            elif owner and not optional_unbound(expected, contexts, spec):
                add(owner, "target-id-source-missing", f"{target.get('bind')}: cannot read unique {spec.get('ref_attribute')!r} ref")
        if target.get("same_id") is True:
            same_id_targets.add(target.get("bind"))
            if len(observed) == sum(not optional_unbound(expected, contexts, spec) for spec in specs) and len(set(observed)) > 1:
                if owner:
                    owner["split_ref_ids"] += 1
                    add(owner, "split-ref-ids", f"{target.get('bind')}: ref ids differ: {observed!r}")
                target_id = None
            else:
                target_id = observed[0] if observed and len(observed) == sum(not optional_unbound(expected, contexts, spec) for spec in specs) else None
        else:
            target_id = observed[0] if observed else None
        if target_id is not None:
            bindings[target.get("bind")] = target_id
            if owner:
                target_rows.append((owner, target, target_id))

    for question in expected.get("query", []):
        qid = question["id"]
        report, context = reports[qid], contexts[qid]
        for truth in question.get("rows", []):
            row = context["bound_rows"].get(truth.get("bind"))
            refs = ref_rows(row.get("record")) if isinstance(row, dict) else None
            if refs is None:
                continue
            for ref in truth.get("refs", []):
                matches = [r for r in refs if isinstance(r, dict) and r.get("attribute") == ref.get("attribute")]
                if len(matches) != 1:
                    continue
                actual_id = matches[0].get("id")
                target_bind = ref.get("target_bind")
                expected_id = bindings.get(target_bind)
                if expected_id is None:
                    add(report, "ref-target-unavailable", f"{qid}/{truth.get('bind')}: target {target_bind!r} is not uniquely bound", "COULD-NOT-JUDGE")
                elif actual_id != expected_id:
                    add(report, "ref-target-wrong", f"{qid}/{truth.get('bind')}: {ref.get('attribute')} points to {actual_id!r}, expected {expected_id!r}")
                distinct = ref.get("distinct_from")
                if distinct is not None:
                    other_id = bindings.get(distinct)
                    if other_id is None:
                        add(report, "distinct-ref-unavailable", f"{qid}/{truth.get('bind')}: target {distinct!r} unavailable", "COULD-NOT-JUDGE")
                    elif actual_id == other_id:
                        report["falsemerged_ref_ids"] += 1
                        add(report, "falsemerged-ref-ids", f"{qid}/{truth.get('bind')}: {ref.get('attribute')} shares id with {distinct!r}")
        for constraint in ("distinct_case_ids", "distinct_row_ids"):
            if question.get(constraint) is True:
                ids = [bindings.get(row.get("bind")) for row in question.get("rows", []) if not (is_optional(row) and row.get("bind") not in context["bound_rows"])]
                if all(isinstance(value, str) for value in ids) and len(set(ids)) != len(ids):
                    report["falsemerged_row_ids"] += 1
                    add(report, "falsemerged-row-ids", f"{qid}: {constraint} observed ids {ids!r}")
        same_subject = question.get("same_subject_id", question.get("shared_subject_id"))
        if isinstance(same_subject, str) and same_subject not in same_id_targets:
            ids = []
            for truth in question.get("rows", []):
                row = context["bound_rows"].get(truth.get("bind"))
                refs = ref_rows(row.get("record")) if isinstance(row, dict) else None
                subjects = [r.get("id") for r in refs if isinstance(r, dict) and r.get("attribute") == "subject"] if refs else []
                if len(subjects) == 1 and isinstance(subjects[0], str):
                    ids.append(subjects[0])
            judged = [row for row in question.get("rows", []) if not (is_optional(row) and row.get("bind") not in context["bound_rows"])]
            if len(ids) == len(judged) and len(set(ids)) > 1:
                report["split_ref_ids"] += 1
                add(report, "split-subject-refs", f"{qid}: subject ids for {same_subject!r} differ: {ids!r}")

    for report, target, target_id in target_rows:
        label = f"lookup target {target.get('bind')}"
        atom = lookup["by_id"].get(target_id)
        if atom is None:
            if lookup["complete"]:
                add(report, "target-lookup-missing", f"{label}: id {target_id!r} absent from complete atoms_lookup", bucket="lookup")
            else:
                add(report, "target-lookup-unavailable", f"{label}: id {target_id!r} not in incomplete atoms_lookup", "COULD-NOT-JUDGE", "lookup")
            continue
        if atom.get("type", MISSING) != target.get("type"):
            add(report, "target-type-wrong", f"{label}: type {atom.get('type', '<missing>')!r}, expected {target.get('type')!r}")
        check_attrs(atom.get("attributes", MISSING), target.get("attributes", {}), report, label)
        fields = expected.get("lookup", {}).get("target_fields", [])
        for field in fields:
            actual_field = "evidence_chunks" if field == "evidence" else field
            if actual_field not in atom:
                add(report, "target-field-missing", f"{label}: atoms_lookup lacks {actual_field!r}")
        check_evidence(atom.get("evidence_chunks", MISSING), target.get("evidence", []), chunks, report, label)
        identity = target.get("identity_source")
        if isinstance(identity, dict):
            evidence_ids = atom.get("evidence_chunks", [])
            cites_source = any(chunks.get(cid, {}).get("state") == "ok" and chunks[cid]["center"].get("source_doc_id") == identity.get("source_doc_id") for cid in evidence_ids if isinstance(cid, str)) if isinstance(evidence_ids, list) else False
            unresolved = any(chunks.get(cid, {}).get("state") == "COULD-NOT-JUDGE" for cid in evidence_ids if isinstance(cid, str)) if isinstance(evidence_ids, list) else False
            if not cites_source:
                add(report, "identity-source-unavailable" if unresolved else "identity-source-not-cited", f"{label}: no evidence chunk proves source {identity.get('source_doc_id')!r}", "COULD-NOT-JUDGE" if unresolved else "FAILED", "citation")
    for report in questions:
        report["verdict"] = "FAILED" if report["_failed"] else "COULD-NOT-JUDGE" if report["_cnd"] else "PASSED"
    return questions, bindings


def frozen_hashes(run_dir):
    hashes = {}
    path = run_dir / "frozen.sha256"
    if not path.exists():
        return hashes
    for line in path.read_text(encoding="utf-8").splitlines():
        digest, separator, filename = line.partition("  ")
        if separator and len(digest) == 64:
            hashes[Path(filename).name] = digest
    return hashes


def query_execution_status(run_dir, qid, use_validated):
    if use_validated:
        status_path = run_dir / f"{qid}.validated.status"
        if not status_path.exists():
            return None
        try:
            return int(status_path.read_text(encoding="utf-8").strip())
        except ValueError:
            return None
    journey = run_dir / "journey.log"
    if not journey.exists():
        return None
    for line in journey.read_text(encoding="utf-8").splitlines():
        fields = dict(part.split("=", 1) for part in line.split() if "=" in part)
        if fields.get("step") == qid and fields.get("exit", "").isdigit():
            return int(fields["exit"])
    return None


def execution_provenance(folder, expected, run_dir, use_validated, runs):
    captured = frozen_hashes(run_dir)
    entries = {}
    for question in expected.get("query", []):
        qid, filename = question["id"], question["file"]
        source = folder / filename
        current_hash = hashlib.sha256(source.read_bytes()).hexdigest()
        captured_hash = captured.get(filename)
        entries[qid] = {
            "query_file": str(source.relative_to(ROOT)),
            "captured_sha256": captured_hash,
            "current_sha256": current_hash,
            "syntax_revised_since_capture": captured_hash is not None and current_hash != captured_hash,
            "selected_output": runs[qid]["path"],
            "exit_status": query_execution_status(run_dir, qid, use_validated),
        }
    return entries


def atlas_inventory(lookup, chunks):
    atoms = lookup.get("payload", {}).get("atoms", []) if isinstance(lookup.get("payload"), dict) else []
    inventory = []
    for atom in atoms if isinstance(atoms, list) else []:
        if not isinstance(atom, dict):
            continue
        evidence = []
        for chunk_id in atom.get("evidence_chunks", []) if isinstance(atom.get("evidence_chunks"), list) else []:
            chunk = chunks.get(chunk_id, {})
            center = chunk.get("center", {}) if chunk.get("state") == "ok" else {}
            evidence.append({
                "chunk_id": chunk_id,
                "state": chunk.get("state", "NEVER-RAN"),
                "source_doc_id": center.get("source_doc_id"),
            })
        inventory.append({
            "id": atom.get("id"),
            "type": atom.get("type"),
            "attributes": atom.get("attributes"),
            "refs": atom.get("refs"),
            "evidence": evidence,
        })
    return inventory


def expected_row_diagnostics(expected, query_runs, lookup, chunks):
    atoms = lookup.get("payload", {}).get("atoms", []) if isinstance(lookup.get("payload"), dict) else []
    diagnostics = {}
    for question in expected.get("query", []):
        qid = question["id"]
        run = query_runs.get(qid, {})
        output = run.get("output") if isinstance(run.get("output"), dict) else {}
        rows = output.get("rows", []) if isinstance(output.get("rows"), list) else []
        rows_out = []
        for index, row in enumerate(rows):
            record = row.get("record") if isinstance(row, dict) and isinstance(row.get("record"), dict) else {}
            refs = record.get("refs", []) if isinstance(record.get("refs"), list) else []
            evidence = []
            for chunk_id in record.get("evidence_chunks", []) if isinstance(record.get("evidence_chunks"), list) else []:
                chunk = chunks.get(chunk_id, {})
                center = chunk.get("center", {}) if chunk.get("state") == "ok" else {}
                evidence.append({"chunk_id": chunk_id, "source_doc_id": center.get("source_doc_id"), "state": chunk.get("state", "NEVER-RAN")})
            rows_out.append({
                "index": index,
                "atom_id": row.get("atom_id") if isinstance(row, dict) else None,
                "type": record.get("type"),
                "attributes": record.get("attributes"),
                "refs": [{"attribute": ref.get("attribute"), "id": ref.get("id"), "type": ref.get("type")} for ref in refs if isinstance(ref, dict)],
                "evidence": evidence,
            })
        expected_rows = []
        for truth in question.get("rows", []):
            ident = truth.get("id_bind_from", {})
            source_id, quote = ident.get("source_doc_id"), ident.get("quote")
            query_candidates = []
            for index, row in enumerate(rows):
                record = row.get("record") if isinstance(row, dict) and isinstance(row.get("record"), dict) else {}
                ids = record.get("evidence_chunks", []) if isinstance(record.get("evidence_chunks"), list) else []
                matches, _, _ = quote_hits([x for x in ids if isinstance(x, str)], source_id, quote, chunks)
                if matches:
                    query_candidates.append(row.get("atom_id"))
            atlas_candidates = []
            for atom in atoms if isinstance(atoms, list) else []:
                if not isinstance(atom, dict) or atom.get("type") != truth.get("type"):
                    continue
                ids = atom.get("evidence_chunks", []) if isinstance(atom.get("evidence_chunks"), list) else []
                matches, _, _ = quote_hits([x for x in ids if isinstance(x, str)], source_id, quote, chunks)
                if matches:
                    atlas_candidates.append(atom.get("id"))
            expected_rows.append({
                "bind": truth.get("bind"),
                "type": truth.get("type"),
                "identity_source_doc_id": source_id,
                "identity_quote": quote,
                "query_candidate_atom_ids": query_candidates,
                "atlas_candidate_atom_ids": atlas_candidates,
                "omission_boundary": "typed-query" if not query_candidates and atlas_candidates else "atlas-recording-or-evidence" if not atlas_candidates else None,
                "required_evidence": [
                    {
                        "source_doc_id": evidence.get("source_doc_id"),
                        "quote": evidence.get("quote"),
                        "cited_by_query_candidates": [
                            atom_id for atom_id in query_candidates
                            if any(
                                chunk.get("source_doc_id") == evidence.get("source_doc_id")
                                and evidence.get("quote") in chunks.get(chunk.get("chunk_id"), {}).get("center", {}).get("content", "")
                                for row in rows_out if row.get("atom_id") == atom_id
                                for chunk in row.get("evidence", [])
                            )
                        ],
                    }
                    for evidence in truth.get("evidence", [])
                ],
            })
        diagnostics[qid] = {"actual_rows": rows_out, "expected_rows": expected_rows}
    return diagnostics


def run(fixture, run_dir, use_validated=False):
    folder, expected, recipe, docs, queries, initial = read_fixture(fixture)
    truth = audit_truth(fixture, expected, recipe, docs, queries, initial)
    suffix = ".validated.out" if use_validated else ".out"
    query_runs = {q["id"]: read_query_output(run_dir / f"{q['id']}{suffix}") for q in expected.get("query", [])}
    query_provenance = execution_provenance(folder, expected, run_dir, use_validated, query_runs)
    corpus = expected.get("lookup", {}).get("corpus")
    if not isinstance(corpus, str):
        raise ValueError("expected.toml lookup.corpus is missing")
    lookup = parse_lookup(run_dir / "atoms_lookup.jsonl", corpus)
    chunks, chunk_findings = inspect_chunks(corpus, chunk_ids_from(query_runs, lookup))
    questions, bindings = score(expected, queries, query_runs, lookup, chunks, {tuple(key) for key in truth["invalid_requirements"]})
    diagnostics = expected_row_diagnostics(expected, query_runs, lookup, chunks)
    for report in questions:
        report.update(diagnostics[report["id"]])
        for problem in lookup["findings"] + chunk_findings:
            verdict = problem["verdict"]
            bucket = "lookup" if problem in lookup["findings"] else "citation"
            add(report, problem["code"], problem["message"], verdict, bucket)
        for problem in truth["findings"]:
            if problem.get("query_id") == report["id"] or problem.get("query_id") is None:
                add(report, problem["code"], problem["message"], "COULD-NOT-JUDGE", "truth")
        report["verdict"] = "FAILED" if report["_failed"] else "COULD-NOT-JUDGE" if report["_cnd"] else "NEVER-RAN" if any(x["verdict"] == "NEVER-RAN" for x in report["findings"]) else "PASSED"
        del report["_failed"], report["_cnd"]
    statuses = [q["verdict"] for q in questions]
    overall = "FAILED" if "FAILED" in statuses else "NEVER-RAN" if statuses and all(x == "NEVER-RAN" for x in statuses) else "COULD-NOT-JUDGE" if truth["status"] == "invalid" or any(x in {"COULD-NOT-JUDGE", "NEVER-RAN"} for x in statuses) else "PASSED"
    totals = {}
    for field in ("expected_rows_present", "omitted_expected_rows", "extra_rows"):
        values = [q[field] for q in questions]
        totals[field] = None if any(value is None for value in values) else sum(values)
    for field in ("bad_or_missing_values", "citation_failures", "lookup_disagreements", "query_surface_failures", "split_ref_ids", "falsemerged_ref_ids", "split_row_ids", "falsemerged_row_ids", "matched_count_mismatches"):
        totals[field] = sum(q[field] for q in questions)
    return {
        "fixture": fixture, "corpus": corpus, "run_dir": str(run_dir), "verdict": overall,
        "query_result_selection": "validated" if use_validated else "captured",
        "query_provenance": query_provenance,
        "truth_review": truth,
        "atoms_lookup": {key: lookup[key] for key in ("state", "complete", "truncated", "tool_ids", "findings")},
        "chunk_inspection": {"endpoint": f"{BASE_URL}/v1/corpora/{{corpus}}/chunks/{{chunk_id}}?radius=0", "requested": len(chunks), "states": dict(Counter(record["state"] for record in chunks.values())), "findings": chunk_findings},
        "atlas_inventory": atlas_inventory(lookup, chunks),
        "questions": questions, "totals": totals, "row_bindings": bindings,
    }


def synthetic_case():
    expected = {"lookup": {"corpus": "synthetic", "assert_same_fields": ["id", "type", "attributes", "refs", "evidence"], "target_fields": ["id", "type", "attributes"]}, "query": [
        {"id": "q1", "question": "List the case.", "expected_matched": 1, "rows": [{"bind": "case.one", "type": "case", "attributes": {}, "id_bind_from": {"source_doc_id": "doc-a", "quote": "Case quote exact."}, "refs": [], "evidence": [{"source_doc_id": "doc-a", "quote": "Case quote exact."}]}]},
        {"id": "q2", "question": "List the state.", "expected_matched": 1, "rows": [{"bind": "state.one", "type": "case_state", "attributes": {"state": "confirmed"}, "id_bind_from": {"source_doc_id": "doc-a", "quote": "State quote exact."}, "refs": [{"attribute": "subject", "target_type": "case", "target_bind": "case.one"}], "evidence": [{"source_doc_id": "doc-a", "quote": "State quote exact."}], "attribute_evidence": [{"attribute": "state", "source_doc_id": "doc-a", "quote": "State quote exact."}]}]},
    ]}
    case = {"id": "entity-1", "type": "case", "name": "Opening sentence, not the binding passage.", "attributes": {}, "refs": [], "evidence_chunks": ["chunk-case"]}
    state = {"id": "claim-1", "type": "case_state", "name": "Opening sentence, not the binding passage.", "attributes": {"state": "confirmed"}, "refs": [{"attribute": "subject", "id": "entity-1", "type": "case"}], "evidence_chunks": ["chunk-state"]}
    queries = {"q1": {"target_type": "case"}, "q2": {"target_type": "case_state"}}
    outputs = {
        "q1": {"state": "OUTPUT", "output": {"hit": True, "matched": 1, "rows": [{"atom_id": "entity-1", "attributes": {}, "evidence": ["chunk-case"], "record": case}]}},
        "q2": {"state": "OUTPUT", "output": {"hit": True, "matched": 1, "rows": [{"atom_id": "claim-1", "attributes": {"state": "confirmed"}, "evidence": ["chunk-state"], "record": state}]}},
    }
    lookup = {"state": "complete", "complete": True, "by_id": {"entity-1": dict(case), "claim-1": dict(state)}, "findings": []}
    chunks = {"chunk-case": {"state": "ok", "center": {"source_doc_id": "doc-a", "content": "Case quote exact."}}, "chunk-state": {"state": "ok", "center": {"source_doc_id": "doc-a", "content": "State quote exact. Confirmed."}}}
    return expected, queries, outputs, lookup, chunks


def synthetic_ref_case():
    rows = []
    refs = [
        ("report-a", "Quote A.", "claim-a", "event-1", "fire.shared", None),
        ("report-b", "Quote B.", "claim-b", "event-1", "fire.shared", None),
        ("report-c", "Quote C.", "claim-c", "event-2", "fire.other", "fire.shared"),
    ]
    expected_rows, query_rows, atoms, chunks = [], [], {}, {}
    for source_id, text, atom_id, target_id, bind, distinct in refs:
        chunk_id = "chunk-" + source_id
        ref = {"attribute": "subject", "type": "happening", "target_type": "happening", "target_bind": bind}
        if distinct:
            ref["distinct_from"] = distinct
        record = {"id": atom_id, "type": "report", "attributes": {}, "refs": [{"attribute": "subject", "id": target_id, "type": "happening"}], "evidence_chunks": [chunk_id]}
        expected_rows.append({"bind": source_id, "type": "report", "attributes": {}, "id_bind_from": {"source_doc_id": source_id, "quote": text}, "refs": [ref], "evidence": [{"source_doc_id": source_id, "quote": text}]})
        query_rows.append({"atom_id": atom_id, "attributes": {}, "evidence": [chunk_id], "record": record})
        atoms[atom_id] = dict(record)
        chunks[chunk_id] = {"state": "ok", "center": {"source_doc_id": source_id, "content": text}}
    expected = {"lookup": {"corpus": "synthetic-ref", "target_fields": ["id", "type", "attributes"], "targets": [
        {"bind": "fire.shared", "type": "happening", "same_id": True, "id_from": [{"query_id": "q", "row_bind": "report-a", "ref_attribute": "subject"}, {"query_id": "q", "row_bind": "report-b", "ref_attribute": "subject"}], "attributes": {"what": "fire"}},
        {"bind": "fire.other", "type": "happening", "distinct_from": "fire.shared", "id_from": {"query_id": "q", "row_bind": "report-c", "ref_attribute": "subject"}, "attributes": {"what": "fire"}},
    ]}, "query": [{"id": "q", "expected_matched": 3, "rows": expected_rows}]}
    events = {"event-1": {"id": "event-1", "type": "happening", "attributes": {"what": "fire"}, "refs": [], "evidence_chunks": []}, "event-2": {"id": "event-2", "type": "happening", "attributes": {"what": "fire"}, "refs": [], "evidence_chunks": []}}
    atoms.update(events)
    queries = {"q": {"target_type": "report"}}
    outputs = {"q": {"state": "OUTPUT", "output": {"hit": True, "matched": 3, "rows": query_rows}}}
    lookup = {"state": "complete", "complete": True, "by_id": atoms, "findings": []}
    return expected, queries, outputs, lookup, chunks


def synthetic_rows_case(truth_rows, query_rows, expected_matched, contents):
    """One `report` query over chunks `contents` ({chunk_id: (source_doc_id, text)}).
    `truth_rows` are (bind, source_doc_id, quote, optional, attributes); `query_rows`
    are (atom_id, chunk_id, row text, attributes)."""
    rows = []
    for bind, source_id, text, optional, attributes in truth_rows:
        row = {"bind": bind, "type": "report", "attributes": attributes, "id_bind_from": {"source_doc_id": source_id, "quote": text}, "refs": [], "evidence": [{"source_doc_id": source_id, "quote": text}]}
        if optional:
            row["optional"] = True
        rows.append(row)
    expected = {"lookup": {"corpus": "synthetic-rows", "target_fields": ["id", "type", "attributes"]}, "query": [{"id": "q", "expected_matched": expected_matched, "rows": rows}]}
    out, atoms = [], {}
    for atom_id, chunk_id, text, attributes in query_rows:
        record = {"id": atom_id, "type": "report", "text": text, "attributes": attributes, "refs": [], "evidence_chunks": [chunk_id]}
        out.append({"atom_id": atom_id, "attributes": attributes, "evidence": [chunk_id], "record": record})
        atoms[atom_id] = dict(record)
    outputs = {"q": {"state": "OUTPUT", "output": {"hit": True, "matched": len(out), "rows": out}}}
    lookup = {"state": "complete", "complete": True, "by_id": atoms, "findings": []}
    chunks = {chunk_id: {"state": "ok", "center": {"source_doc_id": source_id, "content": text}} for chunk_id, (source_id, text) in contents.items()}
    return expected, {"q": {"target_type": "report"}}, outputs, lookup, chunks


def codes(report):
    return {finding["code"] for finding in report["findings"]}


def self_test_optional_rows():
    """(a) An optional row is scored when a row binds to it, never omitted when
    absent, never extra when present."""
    contents = {"chunk-a": ("doc-a", "Required sentence."), "chunk-b": ("doc-b", "Optional sentence.")}
    truth = [("required", "doc-a", "Required sentence.", False, {"state": "resolved"}), ("maybe", "doc-b", "Optional sentence.", True, {"state": "resolved"})]
    both = [("claim-a", "chunk-a", "Required sentence.", {"state": "resolved"}), ("claim-b", "chunk-b", "Optional sentence.", {"state": "resolved"})]
    absent = synthetic_rows_case(truth, both[:1], [1, 2], contents)
    report = score(*absent, set())[0][0]
    assert report["verdict"] == "PASSED" and report["omitted_expected_rows"] == 0 and report["extra_rows"] == 0, ("optional absent", report)
    present = synthetic_rows_case(truth, both, [1, 2], contents)
    report = score(*present, set())[0][0]
    assert report["verdict"] == "PASSED" and report["extra_rows"] == 0 and report["expected_rows_present"] == 2, ("optional present", report)
    wrong = synthetic_rows_case(truth, both[:1] + [("claim-b", "chunk-b", "Optional sentence.", {"state": "declined"})], [1, 2], contents)
    report = score(*wrong, set())[0][0]
    assert report["verdict"] == "FAILED" and "attribute-wrong" in codes(report), ("optional present with a wrong value", report)
    missing = synthetic_rows_case(truth, both[1:], [1, 2], contents)
    report = score(*missing, set())[0][0]
    assert report["verdict"] == "FAILED" and report["omitted_expected_rows"] == 1 and "expected-row-omitted" in codes(report), ("required absent", report)
    return 4


def self_test_matched_range():
    """(b) expected_matched is an integer or an inclusive [min, max], and the
    truth audit checks it against the required and optional rows."""
    contents = {"chunk-a": ("doc-a", "Required sentence."), "chunk-b": ("doc-b", "Optional sentence.")}
    truth = [("required", "doc-a", "Required sentence.", False, {}), ("maybe", "doc-b", "Optional sentence.", True, {})]
    rows = [("claim-a", "chunk-a", "Required sentence.", {}), ("claim-b", "chunk-b", "Optional sentence.", {})]
    for count in (1, 2):
        case = synthetic_rows_case(truth, rows[:count], [1, 2], contents)
        report = score(*case, set())[0][0]
        assert "matched-wrong" not in codes(report) and report["matched_count_mismatches"] == 0, ("in range", count, report)
    case = synthetic_rows_case(truth, rows, [1, 2], contents)
    case[2]["q"]["output"]["matched"] = 3
    report = score(*case, set())[0][0]
    assert "matched-wrong" in codes(report) and report["verdict"] == "FAILED", ("above range", report)
    docs = {"doc-a": {"id": "doc-a", "body": "Required sentence."}, "doc-b": {"id": "doc-b", "body": "Optional sentence."}}
    recipe = {"enrichment": {"ontology": {"types": [{"name": "report", "kind": "claim", "attributes": []}]}}}
    def audit(matched, optional=True):
        expected = synthetic_rows_case([t[:3] + (optional and t[3],) + t[4:] for t in truth], rows, matched, contents)[0]
        return {f["code"] for f in audit_truth("issues", expected, recipe, docs, {"q": {"target_type": "report"}}, [])["findings"]}
    assert "truth-count-inconsistent" not in audit([1, 2]), "range equal to required..required+optional"
    for bad in ([1, 3], [0, 2], [2, 1], [1], 2, "2"):
        assert "truth-count-inconsistent" in audit(bad), ("range must match the rows", bad)
    assert "truth-count-inconsistent" not in audit(2, optional=False), "integer with no optional rows"
    return 9


def self_test_same_document_rows():
    """(c) Several query rows citing one chunk: an expected row binds to the
    one whose own text holds its id_bind_from quote, else stays ambiguous."""
    body = "Fire sentence. Release sentence. Treated sentence."
    contents = {"chunk-b": ("doc-b", body)}
    truth = [("treated", "doc-b", "Treated sentence.", False, {})]
    two = [("claim-1", "chunk-b", "Release sentence.", {}), ("claim-2", "chunk-b", "Treated sentence.", {})]
    case = synthetic_rows_case(truth, two, 1, contents)
    report = score(*case, set())[0][0]
    assert "row-binding-ambiguous" not in codes(report) and report["extra_rows"] == 1, ("text picks the row; the other is extra", report)
    assert score(*case, set())[1].get("treated") == "claim-2", "bound to the row whose text holds the quote"
    with_optional = synthetic_rows_case(truth + [("release", "doc-b", "Release sentence.", True, {})], two, [1, 2], contents)
    report, bindings = score(*with_optional, set())
    assert report[0]["verdict"] == "PASSED" and bindings == {"treated": "claim-2", "release": "claim-1"}, ("each row to its own text", report, bindings)
    one = synthetic_rows_case(truth + [("release", "doc-b", "Release sentence.", True, {})], [("claim-1", "chunk-b", "Release sentence.", {})], [1, 2], contents)
    report, bindings = score(*one, set())
    assert report[0]["verdict"] == "PASSED" and bindings == {"treated": "claim-1"}, ("an optional row never takes a required row's binding", report, bindings)
    for label, texts in (("no row text holds the quote", ("Release sentence.", "Fire sentence.")), ("two row texts hold the quote", ("Treated sentence.", "Treated sentence. Again."))):
        case = synthetic_rows_case(truth, [("claim-1", "chunk-b", texts[0], {}), ("claim-2", "chunk-b", texts[1], {})], 1, contents)
        report = score(*case, set())[0][0]
        assert "row-binding-ambiguous" in codes(report) and report["verdict"] == "FAILED", (label, report)
    return 5


def self_test():
    expected, queries, outputs, lookup, chunks = synthetic_case()
    def score_control(out, atoms, passages):
        return score(expected, queries, out, atoms, passages, set())
    good = score_control(outputs, lookup, chunks)
    assert [q["verdict"] for q in good[0]] == ["PASSED", "PASSED"], good
    controls = []
    changed = json.loads(json.dumps(chunks)); changed["chunk-state"]["center"]["content"] = "Wrong passage."
    controls.append(("missing quote", score_control(outputs, lookup, changed), "expected-row-omitted"))
    changed_outputs = json.loads(json.dumps(outputs)); changed_outputs["q2"]["output"]["rows"][0]["record"]["name"] = "State quote exact."
    changed_lookup = json.loads(json.dumps(lookup)); changed_lookup["by_id"]["claim-1"]["name"] = "State quote exact."
    canonical_only = score_control(changed_outputs, changed_lookup, changed)
    controls.append(("canonical name is not evidence", canonical_only, "expected-row-omitted"))
    assert canonical_only[0][1]["omitted_expected_rows"] == 1, canonical_only
    changed = json.loads(json.dumps(outputs)); changed["q2"]["output"]["rows"][0]["record"]["refs"][0]["id"] = "wrong-id"
    atoms = json.loads(json.dumps(lookup)); atoms["by_id"]["claim-1"]["refs"][0]["id"] = "wrong-id"
    controls.append(("wrong subject id", score_control(changed, atoms, chunks), "ref-target-wrong"))
    changed = json.loads(json.dumps(outputs)); changed["q2"]["output"]["rows"][0]["attributes"]["state"] = "reported"; changed["q2"]["output"]["rows"][0]["record"]["attributes"]["state"] = "reported"
    atoms = json.loads(json.dumps(lookup)); atoms["by_id"]["claim-1"]["attributes"]["state"] = "reported"
    controls.append(("wrong attributes", score_control(changed, atoms, chunks), "attribute-wrong"))
    changed = json.loads(json.dumps(chunks)); changed["chunk-state"]["center"]["source_doc_id"] = "doc-other"
    controls.append(("lost source", score_control(outputs, lookup, changed), "expected-row-omitted"))
    changed = json.loads(json.dumps(outputs)); changed["q2"]["output"]["matched"] = 2
    controls.append(("duplicate count", score_control(changed, lookup, chunks), "matched-wrong"))
    changed = json.loads(json.dumps(outputs)); del changed["q2"]["output"]["matched"]
    missing = score_control(changed, lookup, chunks)
    assert missing[0][1]["actual_matched"] is None and any(x["code"] == "matched-missing" for x in missing[0][1]["findings"]), missing
    controls.append(("missing count field", missing, "matched-missing"))
    for label, (reports, _), code in controls:
        assert any(q["verdict"] == "FAILED" and any(f["code"] == code for f in q["findings"]) for q in reports), (label, reports)
    refs_expected, refs_queries, refs_outputs, refs_lookup, refs_chunks = synthetic_ref_case()
    refs_good = score(refs_expected, refs_queries, refs_outputs, refs_lookup, refs_chunks, set())
    assert refs_good[0][0]["verdict"] == "PASSED", refs_good
    split_outputs = json.loads(json.dumps(refs_outputs)); split_outputs["q"]["output"]["rows"][1]["record"]["refs"][0]["id"] = "event-2"
    split_lookup = json.loads(json.dumps(refs_lookup)); split_lookup["by_id"]["claim-b"]["refs"][0]["id"] = "event-2"
    split = score(refs_expected, refs_queries, split_outputs, split_lookup, refs_chunks, set())
    assert split[0][0]["split_ref_ids"] == 1 and split[0][0]["verdict"] == "FAILED", split
    merged_outputs = json.loads(json.dumps(refs_outputs)); merged_outputs["q"]["output"]["rows"][2]["record"]["refs"][0]["id"] = "event-1"
    merged_lookup = json.loads(json.dumps(refs_lookup)); merged_lookup["by_id"]["claim-c"]["refs"][0]["id"] = "event-1"
    merged = score(refs_expected, refs_queries, merged_outputs, merged_lookup, refs_chunks, set())
    assert merged[0][0]["falsemerged_ref_ids"] == 1 and merged[0][0]["verdict"] == "FAILED", merged
    audits = {}
    for name in ("issues", "news"):
        _, frozen, recipe, docs, query_inputs, problems = read_fixture(name)
        audits[name] = audit_truth(name, frozen, recipe, docs, query_inputs, problems)
    assert audits["issues"]["findings"] == [], audits["issues"]
    assert audits["news"]["findings"] == [], audits["news"]
    _, issues_expected, issues_recipe, issues_docs, issues_queries, issues_problems = read_fixture("issues")
    synthetic_expected = json.loads(json.dumps(issues_expected))
    synthetic_expected["lookup"]["targets"].append({"bind": "case.reminders", "type": "case", "attributes": {}})
    synthetic_expected["query"][1]["rows"][0]["refs"][0]["key"] = "unsupported-case-key"
    invalid_key_audit = audit_truth("issues", synthetic_expected, issues_recipe, issues_docs, issues_queries, issues_problems)
    invalid_key_findings = [f for f in invalid_key_audit["findings"] if f["code"] == "invalid-frozen-key-requirement"]
    assert invalid_key_audit["status"] == "invalid" and len(invalid_key_findings) == 1, invalid_key_audit
    assert invalid_key_findings[0]["verdict"] == "COULD-NOT-JUDGE", invalid_key_audit
    schema = {"optional rows": self_test_optional_rows(), "expected_matched ranges": self_test_matched_range(), "same-document rows": self_test_same_document_rows()}
    print("self-test: 10/10 deliberate negative controls rejected; 2/2 known-good synthetic rowsets passed")
    print("truth audit: issues and news supported fixtures valid; synthetic unsupported identity key rejected")
    print("schema: " + "; ".join(f"{name} {n}/{n} checks held" for name, n in schema.items()))
    return 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if len(argv) not in {2, 3} or (len(argv) == 3 and argv[2] != "--validated"):
        print(f"usage: {Path(__file__).name} <issues|news> <absolute-run-dir> [--validated] | --self-test", file=sys.stderr)
        return 2
    fixture, run_arg = argv[:2]
    use_validated = len(argv) == 3
    run_dir = Path(run_arg)
    if not run_dir.is_absolute() or not run_dir.is_dir():
        print("run directory must be an existing absolute path", file=sys.stderr)
        return 2
    try:
        report = run(fixture, run_dir, use_validated)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as exc:
        print(f"score_baseline: {exc}", file=sys.stderr)
        return 2
    output = run_dir / "verdict.json"
    temp = run_dir / "verdict.json.tmp"
    temp.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    temp.replace(output)
    print(f"{fixture}: {report['verdict']}")
    print(f"  query results: {report['query_result_selection']}")
    for q in report["questions"]:
        print(f"  {q['id']}: {q['verdict']} matched={q['actual_matched']!r}/{q['expected_matched']!r}; present={q['expected_rows_present']!r} omitted={q['omitted_expected_rows']!r} extra={q['extra_rows']!r}; bad/missing={q['bad_or_missing_values']} query-surface={q['query_surface_failures']} split-ref={q['split_ref_ids']} false-merged-ref={q['falsemerged_ref_ids']}")
        for row in q["expected_rows"]:
            print(f"    {row['bind']}: query={row['query_candidate_atom_ids']!r} atlas={row['atlas_candidate_atom_ids']!r} boundary={row['omission_boundary']!r}")
    for problem in report["truth_review"]["findings"]:
        if problem["code"] == "invalid-frozen-key-requirement":
            print(f"  frozen expectation: {problem['verdict']}: {problem['message']}")
    print(f"  totals: {report['totals']}; verdict={output}")
    return {"PASSED": 0, "FAILED": 1, "COULD-NOT-JUDGE": 2, "NEVER-RAN": 3}[report["verdict"]]


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
