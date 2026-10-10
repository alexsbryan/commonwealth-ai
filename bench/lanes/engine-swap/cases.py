#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build the engine conformance case bank and check that it covers the rows.

    cases.py build --capture <capture.jsonl>... [--synthetic <jsonl>] --out <cases.jsonl> [--per-shape 3]
    cases.py coverage --cases <cases.jsonl>

`build` reads `[engine] capture` files (one model call per line) and the
hand-written synthetic cases, assigns each case the rows whose `when`
predicate in conformance.toml matches it, and keeps at most `--per-shape`
cases per distinct set of rows: two cases that exercise the same rows are
interchangeable for the battery. `coverage` exits 1 when a row that has a
`when` has no case.

A predicate is a list of tables, any one matching. Keys are request fields
or the derived keys below; values are `set`, `unset`, `>0`, `=<json>`,
`!=<json>`, or a plain string meaning equal to it. A table matches a
fault-injected case or a raw prompt only if it names `fault` or `shape`.
"""
import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path

INVENTORY = Path(__file__).with_name("conformance.toml")
STREAM_METHODS = {
    "complete_stream",
    "complete_stream_with_id",
    "complete_stream_with_finish",
    "complete_stream_with_id_and_finish",
}
FORCED_CHOICE_SENTINEL = "x_forced_choice"  # oicp_types::forced_choice::SENTINEL


def is_set(v):
    return v not in (None, [], {}, "")


def derived(case):
    """The keys a predicate may name besides the request's own fields."""
    method, inp = case["method"], case.get("input") or {}
    shape = inp.get("prompt_shape")
    if isinstance(shape, dict) and "conversation" in shape:
        shape_name = "conversation"
        messages = shape["conversation"].get("messages") or []
        last_role = messages[-1].get("role") if messages else None
    else:
        shape_name = shape if shape in ("raw", "templated") else "templated"
        last_role = None
    so = inp.get("structured_output")
    return {
        "method": method,
        "kind": "complete" if method.startswith("complete") else method,
        "stream": method in STREAM_METHODS,
        "shape": shape_name,
        "last_role": last_role,
        "forced_choice": isinstance(so, dict) and so.get(FORCED_CHOICE_SENTINEL) is True,
        "schema_bare_object": so == {"type": "object"},
        "tool_choice_named": isinstance(inp.get("tool_choice"), dict),
        "fault": case.get("fault"),
        "label": case.get("label"),
    }


def literal(text):
    """A JSON literal, or the text itself when it is not one: `=assistant`."""
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def holds(cond, value):
    if cond == "set":
        return is_set(value)
    if cond == "unset":
        return not is_set(value)
    if cond == ">0":
        return isinstance(value, (int, float)) and not isinstance(value, bool) and value > 0
    if cond.startswith("!="):
        return value != literal(cond[2:])
    if cond.startswith("="):
        return value == literal(cond[1:])
    # Any other string is a plain value: `shape = "templated"`.
    return value == cond


# Special cases are opted into by name: a table that does not mention these
# keys does not match a fault-injected case or a raw (FIM) prompt, so a FIM
# refusal or an injected error is never counted against an unrelated row.
IMPLICIT = {"fault": "unset", "shape": "!=raw"}


def matches(when, case):
    fields = {**(case.get("input") or {}), **derived(case)}
    return any(
        all(holds(c, fields.get(k)) for k, c in {**IMPLICIT, **table}.items())
        for table in when
    )


def case_id(case):
    canon = json.dumps({"method": case["method"], "input": case.get("input"), "fault": case.get("fault")}, sort_keys=True)
    return hashlib.sha256(canon.encode()).hexdigest()[:12]


def read_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def build(args, rows):
    sources = [(c, "captured") for p in args.capture for c in read_jsonl(p)]
    if args.synthetic:
        sources += [(c, "synthetic") for c in read_jsonl(args.synthetic)]
    kept, per_signature, seen = [], {}, set()
    for case, source in sources:
        cid = case_id(case)
        if cid in seen:
            continue
        seen.add(cid)
        applied = sorted(r["id"] for r in rows if "when" in r and matches(r["when"], case))
        signature = (case["method"], tuple(applied))
        # Synthetic cases are written for a row on purpose and always kept.
        if source == "captured" and per_signature.get(signature, 0) >= args.per_shape:
            continue
        per_signature[signature] = per_signature.get(signature, 0) + 1
        kept.append({
            "case_id": cid,
            "source": source,
            "label": case.get("label", case.get("span", "")),
            "method": case["method"],
            "input": case.get("input"),
            "fault": case.get("fault"),
            "rows": applied,
        })
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        for c in kept:
            f.write(json.dumps(c) + "\n")
    print(f"cases: {len(kept)} kept of {len(sources)} read, {len(per_signature)} distinct row sets -> {args.out}")
    return 0


def coverage(args, rows):
    cases = read_jsonl(args.cases)
    counts = {r["id"]: 0 for r in rows if "when" in r}
    for c in cases:
        for rid in c["rows"]:
            if rid in counts:
                counts[rid] += 1
    gaps = [rid for rid, n in counts.items() if n == 0]
    for rid, n in counts.items():
        print(f"{n:5d}  {rid}")
    scenario_only = [r["id"] for r in rows if "when" not in r]
    print(f"\n{len(counts) - len(gaps)}/{len(counts)} rows have a case; no case yet: {gaps}")
    print(f"scenario rows, judged when their instrument exists: {scenario_only}")
    return 1 if gaps else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("--capture", nargs="*", default=[])
    b.add_argument("--synthetic")
    b.add_argument("--out", required=True)
    b.add_argument("--per-shape", type=int, default=3)
    c = sub.add_parser("coverage")
    c.add_argument("--cases", required=True)
    args = ap.parse_args()
    rows = tomllib.loads(INVENTORY.read_text())["row"]
    return build(args, rows) if args.cmd == "build" else coverage(args, rows)


if __name__ == "__main__":
    sys.exit(main())
