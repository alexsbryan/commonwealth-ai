#!/usr/bin/env python3
# sec_facts.py — THE one decider (ARCH §10.6) for SEC XBRL companyfacts:
# (company, concept, period) -> figure-with-basis, or a refusal that states why.
#
# Consumes the concept-normalization registry
# (sovereign-recipes/sec-filings-company/concept-map.toml) and a raw
# companyfacts JSON (data.sec.gov/api/xbrl/companyfacts/CIK##########.json).
# Nothing else in the repo may interpret either file.
#
# Modes:
#   ask     one (concept, period) -> answer or refusal (human or --json)
#   render  write per-concept fact .txt files for corpus ingest, plus the
#           glassbox deliverables _unmapped_concepts.json (filer tags the map
#           does not cover) and _render_manifest.json, plus sec_facts.json —
#           the typed fact sidecar the Rust `sec_facts` tool answers from
#           (installed into the corpus index dir by setup-sec-corpus.sh).
#           The sidecar holds the SAME resolve() outputs as the .txt lines:
#           this module stays THE one decider; Rust only looks up.
#
# This module WRITES the corpus; it never grades the order's bars. The
# B2/B4 judge is scripts/check-sec-corpus.py, which answers from the
# INSTALLED corpus via retrieval only (seat ruling 2026-08-14 — if the
# writer also answered, the bar could not fail).
#
# Refusal posture (ARCH §18.3): absence is REPORTED, never defaulted.
#   - concept not in the map            -> refuse, name it as unmapped
#   - no tag of the chain in the facts  -> refuse, name the chain tried
#   - tag present, period absent        -> refuse, NAME the nearest available
#     period (naming is reporting; its value is never substituted)
# Period matching is on start/end DATES only. The XBRL `frame` label (e.g.
# "CY2025" on Apple's fiscal-2025 fact) is SEC's nearest-calendar bucketing,
# NOT calendar alignment, and is never consulted.
#
# Glassbox: --debug prints every resolution step to stderr — requested concept,
# tag chain, which alias fired (or the filer override), candidate facts, the
# selection, and every refusal reason.

import argparse
import datetime as dt
import json
import sys
import tomllib

DEBUG = False


def dbg(msg: str) -> None:
    if DEBUG:
        print(f"debug: {msg}", file=sys.stderr)


def load_map(path):
    with open(path, "rb") as f:
        return tomllib.load(f)


def load_facts(path):
    with open(path, "rb") as f:
        return json.load(f)


def cik10(facts) -> str:
    return f"{int(facts['cik']):010d}"


def tag_chain(cmap, cik: str, concept: str):
    """Resolve the tag chain for a concept: filer override wins whole."""
    filer = cmap.get("filers", {}).get(f"cik{cik}", {})
    override = filer.get("overrides", {}).get(concept)
    if override:
        dbg(f"concept={concept} filer override chain {override['tags']}")
        return override["tags"], "filer-override"
    entry = cmap.get("concepts", {}).get(concept)
    if entry is None:
        return None, None
    return entry["tags"], "global"


class Refusal(dict):
    pass


def refuse(concept, period, reason, **extra):
    dbg(f"REFUSE concept={concept} period={period}: {reason}")
    return Refusal(refused=True, concept=concept, requested_period=period,
                   reason=reason, **extra)


def parse_period(spec: str):
    """'FY2025' | 'YYYY-MM-DD' (instant) | 'YYYY-MM-DD..YYYY-MM-DD' (duration)."""
    if spec.upper().startswith("FY") and spec[2:].isdigit():
        return ("fy", int(spec[2:]))
    if ".." in spec:
        start, end = spec.split("..", 1)
        dt.date.fromisoformat(start), dt.date.fromisoformat(end)
        return ("duration", start, end)
    dt.date.fromisoformat(spec)
    return ("instant", spec)


def is_annual_10k_fact(e, kind: str) -> bool:
    """A fact reported as a fiscal-year figure in a 10-K: fp=FY, and for
    durations a ~1-year span (330-380 days) so quarterly comparatives never
    masquerade as annual figures."""
    if e.get("form") not in ("10-K", "10-K/A") or e.get("fp") != "FY":
        return False
    if kind == "duration":
        if not e.get("start") or not e.get("end"):
            return False
        days = (dt.date.fromisoformat(e["end"]) - dt.date.fromisoformat(e["start"])).days
        return 330 <= days <= 380
    return not e.get("start") and bool(e.get("end"))


def annual_10k_facts(entries, fy: int, kind: str):
    """Facts for fiscal year N = annual-shaped 10-K facts whose OWN period
    ends in calendar year N. NEVER keyed on the `fy` field: companyfacts
    stamps `fy` with the fiscal year of the FILING, so a 10-K's prior-year
    comparative column carries the CURRENT filing's fy — key on it and the
    wrong year's figure returns confidently (measured: Apple's
    2023-10-01..2024-09-28 revenue appears under both fy=2024 and fy=2025).
    Identity comes from the fact's essence — (start, end, unit) — per ARCH
    §7.5. This also excludes prior-year comparative balance dates for
    instants (their end year is N-1) with no separate guard."""
    return [e for e in entries
            if is_annual_10k_fact(e, kind) and int(e["end"][:4]) == fy]


def exact_period_facts(entries, kind, start, end):
    if kind == "duration":
        return [e for e in entries if e.get("start") == start and e.get("end") == end]
    return [e for e in entries if not e.get("start") and e.get("end") == end]


def nearest_period(entries, kind):
    """Latest available fact, for NAMING in a refusal (never substitution)."""
    dated = [e for e in entries if e.get("end")]
    if not dated:
        return None
    e = max(dated, key=lambda x: (x["end"], x.get("filed", "")))
    return {"start": e.get("start"), "end": e["end"], "fy": e.get("fy"),
            "fp": e.get("fp"), "form": e.get("form")}


def resolve(cmap, facts, concept: str, period_spec: str, unmapped_log=None):
    """THE decider. Returns an answer dict or a Refusal."""
    cik = cik10(facts)
    entity = facts.get("entityName", "?")
    chain, chain_src = tag_chain(cmap, cik, concept)
    if chain is None:
        if unmapped_log is not None:
            unmapped_log.append(concept)
        return refuse(concept, period_spec,
                      f"concept '{concept}' is not in the normalization map — "
                      f"unmapped concepts are reported, never defaulted to a near neighbour")
    kind = cmap["concepts"].get(concept, {}).get("kind") if chain_src == "global" else \
        cmap.get("filers", {}).get(f"cik{cik}", {}).get("overrides", {}).get(concept, {}).get(
            "kind", cmap.get("concepts", {}).get(concept, {}).get("kind"))
    gaap = facts.get("facts", {}).get("us-gaap", {})

    tag = None
    for i, t in enumerate(chain):
        if t in gaap:
            tag = t
            dbg(f"concept={concept} chain={chain} matched tag={t} "
                f"(alias {chain_src}[{i}])")
            break
    if tag is None:
        return refuse(concept, period_spec,
                      f"none of the tags {chain} is present in {entity}'s companyfacts",
                      tags_tried=chain)

    units = gaap[tag].get("units", {})
    try:
        p = parse_period(period_spec)
    except ValueError as e:
        return refuse(concept, period_spec, f"unparseable period spec: {e}")

    candidates = []  # (unit, fact)
    for unit, entries in units.items():
        if p[0] == "fy":
            sel = annual_10k_facts(entries, p[1], kind)
        elif p[0] == "duration":
            if kind != "duration":
                return refuse(concept, period_spec,
                              f"'{concept}' is an instant (balance-sheet) concept; "
                              f"a date-range period does not apply")
            sel = exact_period_facts(entries, "duration", p[1], p[2])
        else:
            if kind != "instant":
                return refuse(concept, period_spec,
                              f"'{concept}' is a duration concept; a single date "
                              f"names an instant — pass a start..end range or FY<year>")
            sel = exact_period_facts(entries, "instant", None, p[1])
        candidates.extend((unit, e) for e in sel)

    dbg(f"concept={concept} tag={tag} period={period_spec} candidates={len(candidates)}")
    if not candidates:
        near = None
        for unit, entries in units.items():
            n = nearest_period(entries, kind)
            if n and (near is None or n["end"] > near["end"]):
                near = n
        reason = (f"{entity} has no {tag} fact for period '{period_spec}'")
        if p[0] == "duration":
            reason += (" — no fact with exactly that start..end exists; the filer's "
                       "fiscal basis differs (the XBRL 'frame' label is nearest-calendar "
                       "bucketing and is never treated as calendar alignment)")
        if near:
            reason += (f". Nearest available period (named, not substituted): "
                       f"{near.get('start') or 'instant'}..{near['end']} "
                       f"(fy={near.get('fy')} {near.get('fp')}, {near.get('form')})")
        return refuse(concept, period_spec, reason, nearest_available=near)

    by_unit = {}
    for unit, e in candidates:
        by_unit.setdefault(unit, []).append(e)
    if len(by_unit) > 1:
        return refuse(concept, period_spec,
                      f"ambiguous: facts exist in multiple units {sorted(by_unit)}")
    unit, sel = next(iter(by_unit.items()))
    distinct_periods = {(e.get("start"), e["end"]) for e in sel}
    if len(distinct_periods) > 1:
        # 53-week transition edge: two annual periods ending in one calendar
        # year. Refusing beats guessing which one the asker means.
        return refuse(concept, period_spec,
                      f"ambiguous: multiple distinct periods match "
                      f"'{period_spec}': {sorted(distinct_periods)}")
    # Provenance rule: the same fact recurs across filings (8-K earnings, 10-Q
    # comparatives, next year's 10-K). 1) Prefer annual-report forms when any
    # exist — this corpus is 10-K based. 2) All values equal -> cite the
    # EARLIEST filing (the original disclosure). 3) Values differ -> a
    # restatement: the LATEST filed supersedes, and the supersession is logged,
    # never silent.
    annual = [e for e in sel if e.get("form") in ("10-K", "10-K/A")]
    if annual:
        sel = annual
    else:
        dbg(f"concept={concept} no 10-K fact for the period; using "
            f"{sorted({e.get('form') for e in sel})}")
    sel.sort(key=lambda e: e.get("filed", ""))
    if len({e["val"] for e in sel}) > 1:
        dbg(f"concept={concept} RESTATED across filings: "
            f"{[(e.get('filed'), e.get('accn'), e['val']) for e in sel]}; "
            f"latest filed supersedes")
        f = sel[-1]
    else:
        f = sel[0]
    if len(sel) > 1:
        dbg(f"concept={concept} {len(sel)} facts for the period; citing "
            f"filed={f.get('filed')} accn={f.get('accn')}")
    label = (cmap.get("concepts", {}).get(concept, {}) or {}).get("label", concept)
    return {
        "refused": False,
        "entity": entity, "cik": cik,
        "concept": concept, "label": label, "tag": f"us-gaap:{tag}",
        "value": f["val"], "unit": unit,
        "period": {"start": f.get("start"), "end": f["end"]},
        # Fiscal-year label from the fact's OWN end date (never the `fy`
        # field, which names the filing, not the period).
        "basis": (f"fiscal year FY{f['end'][:4]} ({f['start']} to {f['end']})"
                  if f.get("start") else f"as of {f['end']} (fiscal FY{f['end'][:4]} balance date)"),
        "accession": f.get("accn"), "form": f.get("form"), "filed": f.get("filed"),
    }


def fts_tag(tag: str) -> str:
    """Render an XBRL tag for corpus text. The FTS index drops tokens over
    ~40 chars (tantivy RemoveLongFilter), so long CamelCase tags are
    word-split at camel boundaries — concatenating the words recovers the
    exact tag. Short tags stay verbatim."""
    if len(tag) < 40:
        return f"us-gaap:{tag}"
    import re as _re
    return "us-gaap: " + " ".join(_re.findall(r"[A-Z][a-z0-9]*|[a-z0-9]+", tag))


def fmt_value(value, unit):
    if unit == "USD" and isinstance(value, (int, float)) and abs(value) >= 1_000_000:
        return f"${value / 1_000_000:,.0f} million USD (raw: {value:,.0f})"
    return f"{value} {unit}"


# ── modes ────────────────────────────────────────────────────────────────────

def cmd_ask(args):
    cmap, facts = load_map(args.map), load_facts(args.facts)
    r = resolve(cmap, facts, args.concept, args.period)
    if args.json:
        print(json.dumps(r, indent=2))
    elif r.get("refused"):
        print(f"REFUSED: {r['reason']}")
    else:
        print(f"{r['entity']} — {r['label']} [{r['tag']}]: "
              f"{fmt_value(r['value'], r['unit'])}, {r['basis']}; "
              f"source {r['form']} accession {r['accession']} filed {r['filed']}")
    return 0 if not r.get("refused") else 3


def cmd_render(args):
    import os
    cmap, facts = load_map(args.map), load_facts(args.facts)
    cik = cik10(facts)
    entity = facts.get("entityName", "?")
    filer = cmap.get("filers", {}).get(f"cik{cik}", {})
    ticker = filer.get("ticker", args.ticker or "?")
    os.makedirs(args.out, exist_ok=True)
    gaap = facts.get("facts", {}).get("us-gaap", {})

    fys = sorted(args.fy) if args.fy else None
    manifest, rendered_files = [], 0
    sidecar_concepts = {}
    for concept in sorted(cmap.get("concepts", {})):
        years = fys
        if years is None:
            kind = cmap["concepts"][concept]["kind"]
            chain, _ = tag_chain(cmap, cik, concept)
            avail = {e["fy"]
                     for t in chain
                     for entries in gaap.get(t, {}).get("units", {}).values()
                     for e in entries if is_annual_10k_fact(e, kind)}
            years = sorted(avail)[-3:]
        lines, misses, typed = [], [], []
        for fy in years:
            r = resolve(cmap, facts, concept, f"FY{fy}")
            if r.get("refused"):
                misses.append({"fy": fy, "reason": r["reason"]})
                continue
            lines.append(
                f"{r['entity']} ({ticker}, CIK {cik}) — {r['label']} "
                f"[{fts_tag(r['tag'].removeprefix('us-gaap:'))}]: "
                f"{fmt_value(r['value'], r['unit'])} — {r['basis']}. "
                f"Reported in Form {r['form']}, accession {r['accession']}, "
                f"filed {r['filed']}.")
            typed.append({
                # Identity from essence (ARCH §7.5): concept+period+unit+
                # accession; fiscal_year from the fact's OWN end date.
                "value": r["value"], "unit": r["unit"],
                "start": r["period"]["start"], "end": r["period"]["end"],
                "fiscal_year": int(r["period"]["end"][:4]),
                "tag": r["tag"], "accession": r["accession"],
                "form": r["form"], "filed": r["filed"],
            })
        entry = {"concept": concept, "fys": years,
                 "rendered": len(lines), "misses": misses}
        if lines:
            path = os.path.join(args.out, f"facts-{concept}.txt")
            head = (f"{entity} ({ticker}) — {cmap['concepts'][concept]['label']} — "
                    f"XBRL facts from SEC companyfacts, CIK {cik}.\n")
            with open(path, "w", encoding="utf-8") as f:
                f.write(head + "\n".join(lines) + "\n")
            entry["file"] = path
            rendered_files += 1
        if typed:
            sidecar_concepts[concept] = {
                "label": cmap["concepts"][concept]["label"],
                "kind": cmap["concepts"][concept]["kind"],
                "facts": typed,
            }
        manifest.append(entry)

    covered = {t for c in cmap["concepts"] for t in tag_chain(cmap, cik, c)[0]}
    unmapped = sorted(set(gaap) - covered)
    with open(os.path.join(args.out, "_unmapped_concepts.json"), "w") as f:
        json.dump({"cik": cik, "entity": entity,
                   "filer_tags_total": len(gaap),
                   "covered_by_map": sorted(covered & set(gaap)),
                   "unmapped": unmapped}, f, indent=2)
    with open(os.path.join(args.out, "_render_manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)

    # The typed fact sidecar (FINANCIAL_CORPORA §6.2): the Rust sec_facts
    # tool answers from THIS file, never from companyfacts — the same
    # resolve() outputs as the .txt lines, so writer-side selection stays
    # in one decider. as_of anchors freshness (F6); coverage states the
    # consolidated-only source limit (F5).
    all_typed = [f for c in sidecar_concepts.values() for f in c["facts"]]
    if all_typed:
        latest = max(all_typed, key=lambda f: (f["filed"], f["end"]))
        sidecar = {
            "schema": 1,
            "entity": entity, "ticker": ticker, "cik": cik,
            "as_of": {"form": latest["form"], "accession": latest["accession"],
                      "filed": latest["filed"],
                      "latest_period_end": max(f["end"] for f in all_typed)},
            "concepts": sidecar_concepts,
            "coverage": {"filer_tags_total": len(gaap),
                         "covered_tags": len(covered & set(gaap)),
                         "unmapped_tags": len(unmapped),
                         "consolidated_only": True},
        }
        with open(os.path.join(args.out, "sec_facts.json"), "w") as f:
            json.dump(sidecar, f, indent=2)

    print(f"rendered {rendered_files} concept files to {args.out}; "
          f"{len(unmapped)}/{len(gaap)} filer tags unmapped "
          f"(named in _unmapped_concepts.json); typed sidecar sec_facts.json "
          f"({len(all_typed)} facts across {len(sidecar_concepts)} concepts)")
    return 0


# ── the rendered-line grammar, in ONE place ──────────────────────────────────
# scripts/check-sec-corpus.py (the retrieval-side judge) imports this parser
# so the writer and the reader of the corpus line format cannot drift. The
# JUDGING of the order's bars lives entirely in the checker — this module
# writes the corpus and answers ad-hoc `ask` queries; it never grades itself
# (seat ruling 2026-08-14: the ingest-side decider must not be the B2/B4
# instrument, or the bar cannot fail).

FACT_LINE_RE = None  # compiled lazily below


def parse_fact_line(line: str):
    """Parse one rendered fact line back into its parts, or None."""
    import re as _re
    global FACT_LINE_RE
    if FACT_LINE_RE is None:
        FACT_LINE_RE = _re.compile(
            r"CIK (?P<cik>\d{10})\) — (?P<label>.+?) \[us-gaap:\s?(?P<tag>.+?)\]: "
            r"(?P<value_text>.+?) — (?:"
            r"fiscal year FY(?P<fy_dur>\d{4}) \((?P<start>\d{4}-\d{2}-\d{2}) to (?P<end>\d{4}-\d{2}-\d{2})\)"
            r"|as of (?P<instant>\d{4}-\d{2}-\d{2}) \(fiscal FY(?P<fy_inst>\d{4}) balance date\)"
            r")\. Reported in Form (?P<form>\S+), accession (?P<accn>\S+), "
            r"filed (?P<filed>\d{4}-\d{2}-\d{2})\.")
    m = FACT_LINE_RE.search(line)
    if not m:
        return None
    d = m.groupdict()
    vt = d["value_text"]
    raw = _re.search(r"\(raw: ([-\d,]+)\)", vt)
    if raw:
        value, unit = float(raw.group(1).replace(",", "")), "USD"
    else:
        vm = _re.match(r"([-\d.,]+) (\S+)", vt)
        if not vm:
            return None
        value, unit = float(vm.group(1).replace(",", "")), vm.group(2)
    return {
        "cik": d["cik"], "label": d["label"],
        "tag": d["tag"].replace(" ", ""),
        "value": value, "unit": unit,
        "period": ({"start": d["start"], "end": d["end"]} if d["fy_dur"]
                   else {"start": None, "end": d["instant"]}),
        "fiscal_year": int(d["fy_dur"] or d["fy_inst"]),
        "form": d["form"], "accession": d["accn"], "filed": d["filed"],
    }


def main():
    global DEBUG
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--debug", action="store_true")
    sub = ap.add_subparsers(dest="mode", required=True)

    a = sub.add_parser("ask")
    a.add_argument("--map", required=True)
    a.add_argument("--facts", required=True)
    a.add_argument("--concept", required=True)
    a.add_argument("--period", required=True)
    a.add_argument("--json", action="store_true")
    a.set_defaults(fn=cmd_ask)

    r = sub.add_parser("render")
    r.add_argument("--map", required=True)
    r.add_argument("--facts", required=True)
    r.add_argument("--out", required=True)
    r.add_argument("--ticker")
    r.add_argument("--fy", type=int, action="append",
                   help="fiscal year(s) to render; default: latest 3 available")
    r.set_defaults(fn=cmd_render)

    args = ap.parse_args()
    DEBUG = args.debug
    sys.exit(args.fn(args))


if __name__ == "__main__":
    main()
