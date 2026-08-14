#!/usr/bin/env python3
# check-sec-corpus.py — the retrieval-side judge for the sec-filings-company
# bars (B2 figures, B4 negative controls). The corpus is the system under
# test: every answer here comes from chunks RETRIEVED from the installed
# corpus over the daemon's /v1/knowledge/search — never from companyfacts
# JSON, and never from the renderer (seat ruling 2026-08-14: if the script
# that writes the corpus also answered, the bar could not fail).
#
# ONE answer rule serves both bars — the honesty property is that the same
# answerer that produces B2's figures must refuse B4's questions:
#   1. retrieve chunks for the question (concept phrasing + period phrasing;
#      expected values NEVER enter the query);
#   2. parse typed fact lines out of the retrieved chunks (the line grammar
#      lives in scripts/sec_facts.py — writer and reader share one parser);
#   3. a figure answer REQUIRES a typed fact line whose concept (tag chain
#      from the shared concept map) AND period match the question exactly.
#      Anything else is a refusal that states why: unmapped concept, or a
#      named nearest-available period — reported, never substituted
#      (ARCH §18.3). Prose chunks are narrative support, not citations.
#
# Verdicts per item: passed / FAILED / could-not-judge (ARCH §18.2).
# Exit: 0 all passed; 2 if any B4 control failed (B4 outranks); 1 otherwise.

import argparse
import json
import sys
import tomllib
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from sec_facts import parse_fact_line  # noqa: E402  (the one line grammar)

DEBUG = False


def dbg(msg):
    if DEBUG:
        print(f"debug: {msg}", file=sys.stderr)


def search(daemon, corpus, query, limit):
    req = urllib.request.Request(
        f"{daemon}/v1/knowledge/search",
        data=json.dumps({"query": query, "corpora": [corpus], "limit": limit}).encode(),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=120) as resp:
        return json.load(resp)["results"]


def humanize(concept_id):
    return concept_id.replace("_", " ")


def camel_words(tag):
    import re as _re
    return _re.findall(r"[A-Z][a-z0-9]*|[a-z0-9]+", tag)


def build_queries(cmap, concept, period):
    """Value-free query variants; results are unioned before the match rule.

    - The period never enters a query: it is a FILTER applied in the match
      rule, not a retrieval cue — every fact chunk carries 'fiscal year
      FY<n>' lines, so period words match everything and dilute the concept
      terms (measured: adding 'fiscal year 2025 (FY2025)' pushed
      facts-revenue out of top-8; label-only ranks it first).
    - The tag-token variant recovers chunks the hybrid ranker under-ranks
      for multi-word labels (measured: 'Advertising expense' missed
      facts-advertising_expense in top-12; the single token
      'AdvertisingExpense' ranks it first). Tags the renderer word-splits
      (>=40 chars) are split here the same way, or they cannot match FTS.
    - Expected values NEVER appear in any variant."""
    entry = cmap.get("concepts", {}).get(concept)
    if entry is None:
        return [humanize(concept)]
    tag_terms = []
    for t in entry["tags"]:
        tag_terms.append(t if len(t) < 40 else " ".join(camel_words(t)))
    return [entry["label"], " ".join(tag_terms)]


def period_matches(line_period, period_spec, kind):
    if period_spec.upper().startswith("FY"):
        year = period_spec[2:]
        if kind == "instant":
            return line_period["start"] is None and line_period["end"][:4] == year
        return line_period["start"] is not None and line_period["end"][:4] == year
    if ".." in period_spec:
        start, end = period_spec.split("..", 1)
        return line_period == {"start": start, "end": end}
    return line_period["start"] is None and line_period["end"] == period_spec


def retrieve(args, cmap, concept, period):
    """Union of the query variants' results, deduped by chunk identity."""
    seen, union = set(), []
    for q in build_queries(cmap, concept, period):
        dbg(f"query variant: {q!r}")
        for r in search(args.daemon, args.corpus, q, args.limit):
            key = (r.get("corpus_id"), r.get("chunk_id"), r.get("title"))
            if key not in seen:
                seen.add(key)
                union.append(r)
    return union


def answer(cmap, results, concept, period):
    """The answer rule. Returns an answer dict or a refusal dict."""
    entry = cmap.get("concepts", {}).get(concept)
    fact_lines = []
    for r in results:
        for ln in r["content"].splitlines():
            f = parse_fact_line(ln)
            if f:
                f["_chunk"] = r.get("title")
                fact_lines.append(f)
    dbg(f"concept={concept} period={period}: {len(results)} chunks retrieved, "
        f"{len(fact_lines)} typed fact lines parsed")

    if entry is None:
        return {"refused": True, "reason":
                f"concept '{concept}' is not in the normalization map — no typed "
                f"fact in this corpus carries it (companyfacts is consolidated-"
                f"only; dimensional figures are deliberately unmapped). Retrieved "
                f"{len(results)} chunk(s) of narrative/other facts; a figure "
                f"answer requires a typed fact line, so this is refused, not "
                f"approximated from a near neighbour."}

    chain = set(entry["tags"])
    kind = entry["kind"]
    of_concept = [f for f in fact_lines if f["tag"] in chain]
    matches = [f for f in of_concept if period_matches(f["period"], period, kind)]
    dbg(f"concept={concept}: {len(of_concept)} lines of this concept, "
        f"{len(matches)} matching period '{period}'")

    if matches:
        vals = {(f["value"], f["unit"], f["period"]["end"]) for f in matches}
        if len(vals) > 1:
            return {"refused": True, "reason":
                    f"retrieved facts conflict for {concept} {period}: {sorted(vals)}"}
        f = matches[0]
        basis = (f"fiscal year FY{f['period']['end'][:4]} "
                 f"({f['period']['start']} to {f['period']['end']})"
                 if f["period"]["start"] else f"as of {f['period']['end']}")
        return {"refused": False, "value": f["value"], "unit": f["unit"],
                "period": f["period"], "basis": basis,
                "accession": f["accession"], "tag": f["tag"], "chunk": f["_chunk"]}

    if of_concept:
        avail = sorted({f["period"]["end"] for f in of_concept})
        return {"refused": True, "reason":
                f"the corpus carries typed {concept} facts, but none for period "
                f"'{period}' — available period end date(s), named not "
                f"substituted: {avail}. For a fiscal-year filer a calendar-"
                f"aligned or out-of-range period has no matching fact."}
    return {"refused": True, "reason":
            f"no typed fact line for {concept} among {len(results)} retrieved "
            f"chunk(s) — cannot answer with a citation, so refusing"}


def main():
    global DEBUG
    ap = argparse.ArgumentParser()
    ap.add_argument("--prereg", required=True)
    ap.add_argument("--map", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--daemon", default="http://127.0.0.1:9741")
    # Recall stage: the answer rule requires an exact typed match, so a
    # larger k adds candidates, never wrong answers.
    ap.add_argument("--limit", type=int, default=12)
    ap.add_argument("--debug", action="store_true")
    args = ap.parse_args()
    DEBUG = args.debug

    with open(args.prereg, "rb") as f:
        prereg = tomllib.load(f)
    with open(args.map, "rb") as f:
        cmap = tomllib.load(f)
    subj = prereg["subject"]
    rows, b2_fail, b4_fail = [], 0, 0

    for item in prereg.get("b2", []):
        concept, period = item["concept"], item["period"]
        try:
            a = answer(cmap, retrieve(args, cmap, concept, period), concept, period)
        except Exception as e:
            rows.append(("B2", concept, period, "could-not-judge", repr(e)))
            b2_fail += 1
            continue
        if a["refused"]:
            rows.append(("B2", concept, period, "FAILED", f"refused: {a['reason']}"))
            b2_fail += 1
            continue
        pd = subj["period_duration"]
        ok = (a["value"] == item["expected_value"]
              and a["unit"] == item["unit"]
              and a["accession"] == subj["accession"]
              and (a["period"] == {"start": pd["start"], "end": pd["end"]}
                   if period.upper().startswith("FY")
                   else a["period"]["end"] == subj["period_instant"]))
        if ok:
            rows.append(("B2", concept, period, "passed",
                         f"{a['value']:,.10g} {a['unit']}; {a['basis']}; "
                         f"accn {a['accession']} (chunk {a['chunk']})"))
        else:
            rows.append(("B2", concept, period, "FAILED",
                         f"got {a['value']} {a['unit']} period={a['period']} "
                         f"accn={a['accession']} vs expected "
                         f"{item['expected_value']} {item['unit']}"))
            b2_fail += 1

    for item in prereg.get("b4", []):
        concept, period = item["concept"], item["period"]
        try:
            a = answer(cmap, retrieve(args, cmap, concept, period), concept, period)
        except Exception as e:
            rows.append(("B4", concept, period, "could-not-judge", repr(e)))
            b4_fail += 1
            continue
        if a["refused"] and a.get("reason"):
            rows.append(("B4", concept, period, "passed", f"refused: {a['reason']}"))
        else:
            rows.append(("B4", concept, period, "FAILED",
                         f"CONFIDENT NUMBER ON A NEGATIVE CONTROL: "
                         f"{a.get('value')} {a.get('unit')} — fails the order "
                         f"regardless of B2"))
            b4_fail += 1

    w = max(len(r[1]) for r in rows)
    for kind, concept, period, verdict, detail in rows:
        print(f"{kind}  {concept:<{w}}  {period:<24}  {verdict:<15}  {detail}")
    n2, n4 = len(prereg.get("b2", [])), len(prereg.get("b4", []))
    print(f"\nB2: {n2 - b2_fail}/{n2} passed   B4: {n4 - b4_fail}/{n4} refused correctly")
    sys.exit(2 if b4_fail else (1 if b2_fail else 0))


if __name__ == "__main__":
    main()
