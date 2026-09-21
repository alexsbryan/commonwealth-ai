#!/usr/bin/env python3
"""The four re-census numbers the pre-reg's decision tree reads, measured on a
built atlas and the services' own source documents. Read-only; writes
decisions.json into --out.

  N1  D5   deontic filled on claims whose declared type carries it   bar >= 0.70
  N2  fix  `service` attribution on `data_type` atoms                bar >= 0.90
  N3  D3   `data_type` recall vs each service's own policy table      bar >= 0.70 on 2 of 3
  N4  D4   `recipient` recall BY NAME through resolution              bar >= 0.50

Every row prints one of the four verdicts by name — passed / failed /
could-not-judge / never-ran — never two, and a number under its bar is a
RESULT, not a retry.

Instruments, stated because each could be drawn differently:

* N1 reads `deontic` from a claim's top-level field or its `attributes`, over
  claims whose `claim_kind` is declared `kind = "claim"` in the atlas's own
  `ontology.json`. A claim of an undeclared kind is outside the bar.
* N2 counts a `data_type` atom as attributed when `attributes.service` is
  non-empty. Resolution rewrites that value to a `service` entity id, so only
  presence is read, never the value. `by_document` carries the same rate per
  source document, which is what separates a regressed fix from a corpus whose
  sections name no service.
* N3's truth is the FIRST-column cells of a service's policy tables, per the
  row that ordered this. A table qualifies when its column-1 LABEL names data as
  its subject (`DATA_HEADER`), wherever that label sits — Spotify's data tables
  put it in row 1 under a prose header (`DISPLACED_LABEL`). Every table found is
  printed either way, so a could-not-judge can be checked rather than believed,
  and the rule is pinned in the self-test to Spotify's 10 cells — the table the
  2026-09-19 spike measured 10 of 10 against.
  Where only the SECOND column is the data one, its recall is computed too and
  filed under `aux_not_pre_registered` — named, never substituted (ARCH §6).
* N4's truth is the distinct `recipient` names phase 1 emitted for a service
  (each one a span of that service's own text). A truth name counts BY NAME when
  some `recipient` atom's canonical name equals it after normalisation, and
  `by_alias` when it survives only as an alias — the split the head-noun merge
  destroyed in spike 3 ("3 of 13 by name, 10 of 13 with aliases"). Atoms are
  matched atlas-wide, not within the service, so an attribution gap (N2) cannot
  depress N4.
* Matching normalises to lower-case alphanumerics with single spaces. N3 also
  accepts whole-word containment either way with the shorter side at least four
  characters, because a table cell is free text; N4 requires equality, because
  "by name" is the question.
"""
import argparse
import collections
import json
import os
import pathlib
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "spikes", "extraction-census"))
import chapter_doc_map  # noqa: E402

BAR_DEONTIC = 0.70
BAR_SERVICE_ATTR = 0.90
BAR_DATA_TYPE_RECALL = 0.70
BAR_RECIPIENT_RECALL = 0.50
# A column is the data column when its label names data AS ITS SUBJECT: "Data",
# "Personal information", "Category of data", "What data is processed", "Types of
# data we collect". A keyword test is not enough and was tried first — `\bdata\b`
# also matches YouTube's "Why and how we process data", which is a PURPOSE
# column, and `categor(y|ies)` alone matches its ad-monetisation "Category"
# tables. Either one produced a number against the wrong truth (ARCH §7).
DATA_HEADER = re.compile(
    r"^(what\s+)?((type|types|categor(y|ies))\s*(of\s+)?)?(personal\s+)?(data|information)"
    r"(\s+(we\s+|is\s+|are\s+)?(collect|collected|process|processed|use|used|share|shared))?$",
    re.I)
# Spotify's data tables carry a PROSE header ("Collected when you sign up for the
# Spotify Service…") and put the real label in row 1 as a bare "Categories".
# Accepted there and only there: a bare "Category" in a header row is a
# monetisation table (YouTube), while a prose header over a bare "Categories" is
# how a data table is written. `data_table_truth` is pinned to Spotify's 10 cells
# in the self-test, so a could-not-judge elsewhere means the service, not the rule.
DISPLACED_LABEL = re.compile(r"^categor(y|ies)$", re.I)
PROSE_HEADER_WORDS = 6
MAX_TRUTH_WORDS = 8


def norm(s):
    return re.sub(r"\s+", " ", re.sub(r"[^a-z0-9]+", " ", (s or "").lower())).strip()


def contains_word(hay, needle):
    if len(needle) < 4 or not needle:
        return False
    return re.search(rf"(?:^| ){re.escape(needle)}(?:$| )", hay) is not None


def matches_loosely(truth, names):
    t = norm(truth)
    if not t:
        return False
    for n in names:
        if t == n or contains_word(n, t) or contains_word(t, n):
            return True
    return False


# ── the source documents' tables ─────────────────────────────────────────────

CELL_JUNK = re.compile(r"!\[[^\]]*\]\([^)]*\)|\[([^\]]*)\]\([^)]*\)|<br\s*/?>|\*+|`+")


def cell(text):
    return re.sub(r"\s+", " ", CELL_JUNK.sub(lambda m: m.group(1) or " ", text)).strip()


def tables(md):
    """Every markdown table as (header_cells, [row_cells...])."""
    lines = md.splitlines()
    out = []
    i = 0
    while i < len(lines) - 1:
        if lines[i].lstrip().startswith("|") and re.match(r"^\s*\|[\s:\-|]+\|\s*$", lines[i + 1]):
            head = [cell(c) for c in lines[i].strip().strip("|").split("|")]
            rows = []
            j = i + 2
            while j < len(lines) and lines[j].lstrip().startswith("|"):
                rows.append([cell(c) for c in lines[j].strip().strip("|").split("|")])
                j += 1
            out.append((head, rows))
            i = j
            continue
        i += 1
    return out


def service_tables(root, service):
    found = []
    for f in sorted(pathlib.Path(root, service).glob("*.md")):
        for head, rows in tables(f.read_text()):
            found.append({"doc": f.stem, "header": head, "rows": rows})
    return found


def column_label(head, rows, col):
    """(label, displaced) — a table's label for `col`, wherever it actually sits."""
    h = head[col] if len(head) > col else ""
    if h and len(h.split()) <= PROSE_HEADER_WORDS:
        return h, False
    if rows and len(rows[0]) > col:
        return rows[0][col], True
    return "", True


def is_data_column(head, rows, col):
    label, displaced = column_label(head, rows, col)
    if DATA_HEADER.match(label):
        return True, label
    if displaced and DISPLACED_LABEL.match(label):
        return True, label
    return False, label


def data_table_truth(found, col=0):
    """(truth cells, qualifying tables, prose cells dropped) over one service."""
    truth, qualifying, dropped = set(), [], 0
    for t in found:
        ok, label = is_data_column(t["header"], t["rows"], col)
        if not ok:
            continue
        _, displaced = column_label(t["header"], t["rows"], col)
        qualifying.append({"doc": t["doc"], "header": t["header"], "label": label,
                           "label_in_row_1": displaced})
        for r in t["rows"][1:] if displaced else t["rows"]:
            c = cell(r[col]) if len(r) > col else ""
            if not c:
                continue
            if len(c.split()) > MAX_TRUTH_WORDS:   # prose, not a data-type label
                dropped += 1
                continue
            truth.add(c)
    return sorted(truth), qualifying, dropped


# ── the four numbers ─────────────────────────────────────────────────────────


def n1_deontic(claims, declared_claim_kinds):
    scoped = [c for c in claims if c.get("claim_kind") in declared_claim_kinds]
    filled = [c for c in scoped if (c.get("deontic") or (c.get("attributes") or {}).get("deontic"))]
    if not scoped:
        return {"verdict": "could-not-judge", "reason": "no claim of a declared kind in the atlas",
                "filled": 0, "total": 0, "rate": None, "bar": BAR_DEONTIC,
                "kinds": sorted(declared_claim_kinds)}
    rate = len(filled) / len(scoped)
    return {"verdict": "passed" if rate >= BAR_DEONTIC else "failed",
            "reason": f"{len(filled)} of {len(scoped)} claims of kind(s) {sorted(declared_claim_kinds)} carry a deontic",
            "filled": len(filled), "total": len(scoped), "rate": round(rate, 4), "bar": BAR_DEONTIC}


def n2_service_attr(data_types, chmap=None):
    if not data_types:
        return {"verdict": "could-not-judge", "reason": "no data_type atom in the atlas",
                "filled": 0, "total": 0, "rate": None, "bar": BAR_SERVICE_ATTR}
    filled = [e for e in data_types if (e.get("attributes") or {}).get("service")]
    rate = len(filled) / len(data_types)
    # Which documents the gap sits in, because "the fix regressed" and "half the
    # atoms come from content-policy sections where no service is named" are
    # different findings and one number cannot tell them apart (ARCH §1).
    by_doc = collections.Counter()
    filled_by_doc = collections.Counter()
    if chmap is not None:
        for e in data_types:
            fa = e.get("first_appearance") or {}
            chunk = fa.get("chunk_id") or ((e.get("evidence") or [{}])[0] or {}).get("chunk_id")
            doc = (chmap.get(chunk) or {}).get("doc") or "?"
            by_doc[doc] += 1
            if (e.get("attributes") or {}).get("service"):
                filled_by_doc[doc] += 1
    return {"verdict": "passed" if rate >= BAR_SERVICE_ATTR else "failed",
            "reason": f"{len(filled)} of {len(data_types)} data_type atoms carry attributes.service",
            "filled": len(filled), "total": len(data_types), "rate": round(rate, 4),
            "bar": BAR_SERVICE_ATTR,
            "by_document": {d: {"filled": filled_by_doc[d], "total": n,
                                "rate": round(filled_by_doc[d] / n, 4)}
                            for d, n in by_doc.most_common()}}


def n3_data_type_recall(services, root, names_by_service, all_names):
    per = {}
    for svc in services:
        found = service_tables(root, svc)
        seen = [{"doc": t["doc"], "header": t["header"],
                 "label_col_1": column_label(t["header"], t["rows"], 0)[0]} for t in found]
        truth, qualifying, dropped = data_table_truth(found, col=0)
        names = names_by_service.get(svc) or all_names
        if not truth:
            # The row's rule is column 1. Where only column 2 is the data column
            # (YouTube's "What data is processed"), its recall is reported BESIDE
            # the could-not-judge and never in place of it (ARCH §6).
            aux_truth, aux_tables, aux_dropped = data_table_truth(found, col=1)
            aux = None
            if aux_tables:
                hit = [x for x in aux_truth if matches_loosely(x, names)]
                aux = {"column": 2, "tables": aux_tables, "truth_cells": len(aux_truth),
                       "prose_cells_dropped": aux_dropped, "matched": len(hit),
                       "recall": round(len(hit) / len(aux_truth), 4) if aux_truth else None,
                       "note": "second column, NOT the pre-registered first-column measurement"}
            per[svc] = {"verdict": "could-not-judge",
                        "reason": ("no table in this service's documents has a column-1 label naming data"
                                   if not qualifying else
                                   f"the qualifying table's column 1 holds no label cell "
                                   f"({dropped} prose cells dropped)"),
                        "tables_found": len(found), "tables_seen": seen,
                        "qualifying": qualifying, "truth_cells": 0, "matched": 0, "recall": None,
                        "aux_not_pre_registered": aux}
            continue
        hit = [x for x in truth if matches_loosely(x, names)]
        recall = len(hit) / len(truth)
        per[svc] = {"verdict": "passed" if recall >= BAR_DATA_TYPE_RECALL else "failed",
                    "reason": f"{len(hit)} of {len(truth)} column-1 cells matched a data_type name or alias",
                    "tables_found": len(found), "tables_seen": seen, "qualifying": qualifying,
                    "truth_cells": len(truth), "matched": len(hit), "recall": round(recall, 4),
                    "prose_cells_dropped": dropped,
                    "missed": [x for x in truth if x not in hit][:20], "aux_not_pre_registered": None}
    passed = [s for s, v in per.items() if v["verdict"] == "passed"]
    judged = [s for s, v in per.items() if v["verdict"] in ("passed", "failed")]
    if len(judged) < 2:
        verdict, reason = "could-not-judge", (
            f"{len(judged)} of {len(services)} services could be judged; the decision needs 2")
    else:
        verdict = "passed" if len(passed) >= 2 else "failed"
        reason = f"{len(passed)} of {len(judged)} judged services at or above {BAR_DATA_TYPE_RECALL}"
    return {"verdict": verdict, "reason": reason, "bar": BAR_DATA_TYPE_RECALL, "per_service": per}


def n4_recipient_recall(sketch_recipients, recipient_atoms):
    atom_names = {norm(e["canonical_name"]) for e in recipient_atoms}
    atom_aliases = {norm(a) for e in recipient_atoms for a in (e.get("aliases") or [])}
    per = {}
    for svc, truth in sorted(sketch_recipients.items()):
        by_name = sorted(t for t in truth if norm(t) in atom_names)
        by_alias = sorted(t for t in truth if norm(t) not in atom_names and norm(t) in atom_aliases)
        lost = sorted(t for t in truth if norm(t) not in atom_names and norm(t) not in atom_aliases)
        if not truth:
            per[svc] = {"verdict": "could-not-judge", "reason": "phase 1 emitted no recipient for this service",
                        "truth": 0, "by_name": 0, "by_alias": 0, "recall_by_name": None}
            continue
        recall = len(by_name) / len(truth)
        per[svc] = {"verdict": "passed" if recall >= BAR_RECIPIENT_RECALL else "failed",
                    "reason": f"{len(by_name)} of {len(truth)} extracted recipient names survive as a named atom",
                    "truth": len(truth), "by_name": len(by_name), "by_alias": len(by_alias),
                    "recall_by_name": round(recall, 4), "lost_entirely": len(lost),
                    "examples_absorbed": (by_alias + lost)[:20]}
    tot = sum(v["truth"] for v in per.values())
    named = sum(v["by_name"] for v in per.values())
    overall = named / tot if tot else None
    return {"verdict": ("could-not-judge" if overall is None
                        else "passed" if overall >= BAR_RECIPIENT_RECALL else "failed"),
            "reason": (f"{named} of {tot} extracted recipient names survive as a named atom"
                       if tot else "phase 1 emitted no recipient"),
            "truth": tot, "by_name": named, "recall_by_name": round(overall, 4) if overall is not None else None,
            "bar": BAR_RECIPIENT_RECALL, "per_service": per}


# ── loading ──────────────────────────────────────────────────────────────────


def load_atlas(corpus, data_root=None):
    root = data_root or os.environ.get("SOVEREIGN_DATA_DIR") or os.path.expanduser("~/.svrnmesh")
    atlas = os.path.join(root, "indexes", corpus, "atlas")
    doc = json.load(open(os.path.join(atlas, "atoms.json")))
    atoms = doc["atoms"] if isinstance(doc, dict) else doc
    ont = json.load(open(os.path.join(atlas, "ontology.json")))
    return atoms, ont, atlas


def declared(ont):
    types = ont.get("policies", {}).get("shape", {}).get("types", [])
    return ({t["name"] for t in types if t.get("kind") == "claim"},
            {t["name"] for t in types if t.get("kind") == "entity"})


def sketch_recipients(corpus, chmap, services, data_root=None):
    root = data_root or os.environ.get("SOVEREIGN_DATA_DIR") or os.path.expanduser("~/.svrnmesh")
    p = os.path.join(root, "enrichment", corpus, "runs", "_phase1_checkpoint.jsonl")
    rows = {}
    for line in open(p):
        r = json.loads(line)
        if r.get("kind") == "success":
            rows[r["chapter_id"]] = r
    per = collections.defaultdict(set)
    for cid, r in rows.items():
        doc_service = ((chmap.get(cid) or {}).get("doc") or " — ").split(" — ")[0]
        for e in (r["extracted"].get("section_extraction") or {}).get("entities_introduced") or []:
            if e.get("entity_type") != "recipient":
                continue
            claimed = ((e.get("attributes") or {}).get("service") or "").strip()
            svc = claimed if claimed in services else doc_service
            if svc in services and (e.get("canonical_name") or "").strip():
                per[svc].add(e["canonical_name"].strip())
    return {s: sorted(per.get(s, ())) for s in services}, p


def entity_service(e, chmap):
    fa = e.get("first_appearance") or {}
    chunk = fa.get("chunk_id") or ((e.get("evidence") or [{}])[0] or {}).get("chunk_id")
    return ((chmap.get(chunk) or {}).get("doc") or " — ").split(" — ")[0]


# ── self-test ────────────────────────────────────────────────────────────────


def self_test():
    claims = [{"claim_kind": "obligation", "attributes": {"deontic": "require"}} for _ in range(8)]
    claims += [{"claim_kind": "obligation", "attributes": {}} for _ in range(2)]
    claims += [{"claim_kind": "gossip", "attributes": {}}]              # undeclared: outside the bar
    r = n1_deontic(claims, {"obligation"})
    assert (r["filled"], r["total"], r["verdict"]) == (8, 10, "passed"), r
    # planted failing input: only 6 of 10 carry a deontic, which is under 0.70
    planted = claims[:6] + [{"claim_kind": "obligation", "attributes": {}} for _ in range(4)]
    bad = n1_deontic(planted, {"obligation"})
    assert bad["verdict"] == "failed" and bad["rate"] == 0.6, bad
    assert n1_deontic([], {"obligation"})["verdict"] == "could-not-judge"

    dts = [{"attributes": {"service": "entity-1"}}] * 9 + [{"attributes": {}}]
    assert n2_service_attr(dts)["verdict"] == "passed", n2_service_attr(dts)
    assert n2_service_attr(dts + [{"attributes": {}}] * 3)["verdict"] == "failed"

    md = ("| Category of data | Why |\n| --- | --- |\n| Email address | login |\n"
          "| **Payment data** | billing |\n\ntext\n\n| Why | What data is processed |\n"
          "| --- | --- |\n| ads | Device identifiers |\n")
    head = tables(md)
    assert len(head) == 2 and head[0][0][0] == "Category of data", head
    assert head[0][1][1][0] == "Payment data", head[0][1]

    # Instrument validation, not a fixture: Spotify's data tables are the ones the
    # 2026-09-19 spike measured 10 of 10 against, and they are the shape this rule
    # has to find — a prose header over a bare "Categories" in row 1. If this
    # assertion ever fails, every could-not-judge below it is uninformative.
    root = pathlib.Path(HERE, "..", "spikes", "fineprint-data", "ota-pga-versions")
    truth, qualifying, _ = data_table_truth(service_tables(root, "Spotify"))
    assert len(truth) == 10, (len(truth), truth)
    assert all(q["label_in_row_1"] for q in qualifying), qualifying
    assert "Usage Data" in truth and "Voice Data" in truth, truth
    n3 = n3_data_type_recall(["X"], root, {}, {"email address"})
    assert n3["verdict"] == "could-not-judge", n3          # X publishes no data table
    assert n3["per_service"]["X"]["tables_found"] >= 1, n3  # ...and its tables are shown anyway
    # planted failing input: a service whose table IS found and whose atoms miss it
    s = n3_data_type_recall(["Spotify"], root, {}, {"usage data"})
    assert s["per_service"]["Spotify"]["verdict"] == "failed", s["per_service"]["Spotify"]
    assert s["per_service"]["Spotify"]["matched"] == 1, s["per_service"]["Spotify"]

    atoms = [{"canonical_name": "ad partners", "aliases": ["partners"]},
             {"canonical_name": "search engines", "aliases": None}]
    n4 = n4_recipient_recall({"X": ["ad partners", "search engines", "payment partners"]}, atoms)
    assert (n4["by_name"], n4["truth"]) == (2, 3), n4
    assert n4["verdict"] == "passed", n4
    # planted failing input: one head noun absorbs three named recipients
    absorbed = n4_recipient_recall(
        {"X": ["ad partners", "payment partners", "authentication partners", "search engines"]},
        [{"canonical_name": "partners",
          "aliases": ["ad partners", "payment partners", "authentication partners"]}])
    assert absorbed["verdict"] == "failed", absorbed
    assert (absorbed["by_name"], absorbed["per_service"]["X"]["by_alias"]) == (0, 3), absorbed
    print("self-test: passed")


# ── main ─────────────────────────────────────────────────────────────────────

def main(a):
    atoms, ont, atlas = load_atlas(a.corpus)
    chmap, chmap_source, services = chapter_doc_map.load(a.corpus, a.out)
    claim_kinds, entity_types = declared(ont)
    claims = [x["data"] for x in atoms if x["atom_type"] == "Claim"]
    ents = [x["data"] for x in atoms if x["atom_type"] == "Entity"]
    data_types = [e for e in ents if e.get("entity_type") == "data_type"]
    recipients = [e for e in ents if e.get("entity_type") == "recipient"]

    names_by_service = collections.defaultdict(set)
    all_names = set()
    for e in data_types:
        ns = {norm(e["canonical_name"])} | {norm(x) for x in (e.get("aliases") or [])}
        all_names |= ns
        names_by_service[entity_service(e, chmap)] |= ns
    truth_recipients, checkpoint = sketch_recipients(a.corpus, chmap, services)

    res = {
        "corpus": a.corpus, "atlas": atlas, "checkpoint": checkpoint,
        "chapter_doc_map": {"source": chmap_source, "chapters": len(chmap), "services": services},
        "declared": {"claim_kinds": sorted(claim_kinds), "entity_types": sorted(entity_types)},
        "atoms_total": len(atoms), "data_type_atoms": len(data_types),
        "recipient_atoms": len(recipients),
        "N1_deontic_fill_D5": n1_deontic(claims, claim_kinds),
        "N2_service_attribution_on_data_type": n2_service_attr(data_types, chmap),
        "N3_data_type_recall_D3": n3_data_type_recall(services, a.ota_root, names_by_service, all_names),
        "N4_recipient_recall_by_name_D4": n4_recipient_recall(truth_recipients, recipients),
    }
    w = 46
    print(f"corpus {a.corpus}  atoms {len(atoms)}  services {services}")
    print(f"map    {chmap_source}")
    for key, label in (("N1_deontic_fill_D5", "N1 D5 deontic fill"),
                       ("N2_service_attribution_on_data_type", "N2 service attribution on data_type"),
                       ("N3_data_type_recall_D3", "N3 D3 data_type recall"),
                       ("N4_recipient_recall_by_name_D4", "N4 D4 recipient recall by name")):
        v = res[key]
        num = v.get("rate", v.get("recall_by_name"))
        shown = "-" if num is None else f"{num:.3f}"
        print(f"{label:<{w}} {v['verdict']:<16} {shown:>6} (bar {v.get('bar', BAR_DATA_TYPE_RECALL)})  {v['reason']}")
        for svc, sv in (v.get("per_service") or {}).items():
            n = sv.get("recall") if "recall" in sv else sv.get("recall_by_name")
            print(f"    {svc:<{w - 4}} {sv['verdict']:<16} {'-' if n is None else f'{n:.3f}':>6}  {sv['reason']}")
            aux = sv.get("aux_not_pre_registered")
            if aux:
                print(f"      aux (not pre-registered): column {aux['column']} — "
                      f"{aux['truth_cells']} label cells, {aux['prose_cells_dropped']} prose cells "
                      f"dropped, recall {aux['recall']} — {aux['note']}")
    os.makedirs(a.out, exist_ok=True)
    json.dump(res, open(os.path.join(a.out, "decisions.json"), "w"), indent=1, ensure_ascii=False)
    return res


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", default="ei7-recensus-fineprint")
    ap.add_argument("--out", default=HERE)
    ap.add_argument("--ota-root", default=os.path.join(HERE, "..", "spikes", "fineprint-data",
                                                      "ota-pga-versions"))
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        self_test()
    else:
        main(a)
