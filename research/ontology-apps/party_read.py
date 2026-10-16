"""Party leg read (cast-probe/PREREG.md amendment 3): runs/party's Ward company decisions from the recorded
resolve_decisions.jsonl and trace, and arm O (`name_overlap`) replayed through the gated estimator port.

    party_read.py [run dir]      # default runs/party/ours/ward-party; writes cast-probe/party.json
"""
import collections
import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import cast_probe as P  # noqa: E402  gold_companies(): the Ward scorer's rule over written text
import estimator_replay as R  # noqa: E402  the gated port: events(), run_arms(), bars()

SEEDED = re.compile(r"atlas/resolve: a source record seeded type=(\S+) record=(\S+) name=(\S+) keys=")
ATOM = re.compile(r"atlas/resolve: record becomes an atom type=(\S+) record=(.*?) atom=(\S+) statements=")
WORDS = re.compile(r'#"l[\d-]+:(.*)"$')


def words(stmt):
    m = WORDS.search(stmt)
    return m[1] if m else ""


def identity_tokens(name):
    """A seeded record's declared identity values as tokens: its domain split at `.` and `-`, and its display name."""
    return {t for t in re.split(r"[.-]", name.lower()) + P.TOKEN.findall(name.lower()) if t}


def overlap_arm(type_of, seeded):
    """Arm O: `name_overlap` on each company statement-alternative pair whose alternative holds a declared identity
    value; agrees when the statement's folded word tokens share one with it."""
    def arm(stmt, alts, comps, rec_docs):
        if type_of.get(stmt) != "company":
            return comps
        toks = set(P.TOKEN.findall(words(stmt).lower()))
        return [dict(c, name_overlap=bool(toks & seeded[r])) if r in seeded else c for c, r in zip(comps, alts)]
    return arm


def read(run):
    lines = [R.ANSI.sub("", raw) for raw in open(run / "job.debug.log", errors="replace") if "[resolve:err]" in raw]
    seeded_name = {m[2]: m[3] for line in lines if (m := SEEDED.search(line)) and m[1] == "company"}
    seeded = {r: identity_tokens(n) for r, n in seeded_name.items()}
    atom_of = {m[2]: m[3] for line in lines if (m := ATOM.search(line)) and m[1] == "company"}
    rows = [json.loads(l) for l in open(run / "recorded/resolve_decisions.jsonl")]
    type_of = {o["statement"]: r["type"] for r in rows for o in r["outcomes"]}
    outcome = {o["statement"]: o for r in rows if r["type"] == "company" for o in r["outcomes"]}
    alts_of = {ev[2]: ev[3] for ev in R.events(lines) if ev[0] == "weighed"}

    gates, decisions, finals = R.run_arms(R.events(lines), {"logged": lambda s, a, comps, r: comps,
                                                            "O": overlap_arm(type_of, seeded)}, R.bars(run))
    company_final = next(e for s, e in finals if type_of.get(s) == "company")

    g, comp, forms, ours, resolves, partly = P.gold_companies()
    strict = {s: resolves(words(s)) for s in outcome}
    loose = {s: {c for c in comp if partly(words(s), c)} - strict[s] for s in outcome}
    header_parts = {r: resolves(n) for r, n in seeded_name.items()}

    def argmax(s):
        ch = outcome[s].get("choice") or {}
        if not ch.get("candidates"):
            return None
        best = max(ch["candidates"], key=lambda x: x[1])
        return best[0] if best[1] > (ch.get("none") or 0) else None

    def group(s):
        """The unit a statement is, and its gold companies: strict (the scorer's rule) or partly (a shared token or
        domain stem), Enron (the declared `ours` set) apart."""
        if strict[s]:
            return ("enron" if strict[s] & ours else "strict"), strict[s]
        if loose[s] & ours:
            return "partly_enron", loose[s] & ours
        if loose[s]:
            return "partly", loose[s]
        return None, set()

    def landing(record, linked, target):
        if linked is None:
            return "opened"
        if linked in seeded:
            return "right header" if header_parts[linked] & target else "other header"
        return "mention record"

    units = collections.defaultdict(collections.Counter)
    enron_landing = collections.defaultdict(collections.Counter)
    arms = {a: collections.Counter() for a in ("logged", "O")}
    rec_parts = collections.defaultdict(set, {r: set(p) for r, p in header_parts.items()})
    rec_stmts = collections.defaultdict(list)
    linked_by_stmt = {}
    for stmt, decision, record, linked_by in decisions:
        if type_of.get(stmt) != "company":
            continue
        linked_by_stmt[stmt] = linked_by
        kind, target = group(stmt)
        logged_link = record if decision != "opened" else None
        if kind:
            right = {r for r in seeded if header_parts[r] & target}
            offered = right & set(alts_of.get(stmt, []))
            a = units[kind]
            a["units"] += 1
            a["right_header_seeded"] += bool(right)
            a["offered"] += bool(offered)
            # diagnostic added after the bar was read: a seeded right header not offered, that a domain/name token
            # shared with the statement's words would reach
            a["not_offered_token_reach"] += bool(right and not offered and {r for r in right if seeded[r] & set(
                P.TOKEN.findall(words(stmt).lower()))})
            if offered:
                a["linked_right_logged"] += record in right
                a["argmax_right"] += argmax(stmt) in right
                a["weigher_override"] += argmax(stmt) in right and record not in right
                a["linked_right_O"] += linked_by["O"] in right
                a["linked_right_replay_logged"] += linked_by["logged"] in right
            if kind in ("enron", "partly_enron"):
                enron_landing[kind][("logged", landing(record, logged_link, target))] += 1
                enron_landing[kind][("O", landing(record, linked_by["O"], target))] += 1
        ps = strict[stmt]
        for arm, linked in (("logged", logged_link), ("O", linked_by["O"])):
            if linked is None:
                continue
            c = arms[arm]
            c["links"] += 1
            c["to header"] += linked in seeded
            if not ps:
                c["unjudged"] += 1
            elif ps & rec_parts[linked]:
                c["right"] += 1
            else:
                c["wrong"] += 1
                c["wrong, record of another gold company"] += bool(rec_parts[linked])
        rec_parts[record] |= ps
        rec_stmts[record].append(stmt)

    # served counterparties marked unjudged, by the record each value is; first order under O, a value goes when the
    # statement that opened its record links to a header record instead
    record_of = {v: k for k, v in atom_of.items()} | {r: r for r in seeded}
    served = collections.Counter()
    for r in map(json.loads, open(run / "recorded/derived_decisions.jsonl")):
        if r.get("attribute") != "counterparty" or r["outcome"] != "decided":
            continue
        for v in r.get("values", []):
            rec = record_of.get(v)
            held = rec_stmts.get(rec, [])
            kinds = {group(s)[0] for s in held}
            enron = "enron" if "enron" in kinds else ("partly_enron" if "partly_enron" in kinds else None)
            unjudged = v in (r.get("unjudged") or [])
            served[("served", "header" if rec in seeded else "mention", "unjudged" if unjudged else "judged", enron)] += 1
            if enron and unjudged:
                opener = linked_by_stmt.get(rec, {}).get("O")
                served[(enron, "unjudged", "logged")] += 1
                served[(enron, "unjudged", "O")] += not (opener in seeded)

    tally = collections.Counter()
    for s, o in outcome.items():
        ch = o.get("choice") or {}
        tally["statements"] += 1
        tally["decision " + (o["outcome"].get("decided") or {}).get("decision", "other")] += 1
        tally["asked"] += bool(ch.get("candidates"))
        top = argmax(s)
        tally["model picked a candidate"] += top is not None
        tally["model picked a header"] += top in seeded
        if top is not None:
            dec = (o["outcome"].get("decided") or {})
            tally["picked, linked to the pick"] += dec.get("record") == top and dec.get("decision") != "opened"
            tally["picked header, linked to it"] += top in seeded and dec.get("record") == top
    tally = {k: v for k, v in tally.items() if v}

    def per_unit(a):
        return {k: a[k] for k in ("units", "right_header_seeded", "offered", "not_offered_token_reach",
                                  "linked_right_logged", "argmax_right", "weigher_override",
                                  "linked_right_replay_logged", "linked_right_O")}

    o_rate = {k: units[k]["linked_right_O"] / units[k]["offered"] if units[k]["offered"] else None
              for k in ("strict", "partly")}
    enron_u = {k: (served[(k, "unjudged", "logged")], served[(k, "unjudged", "O")]) for k in ("enron", "partly_enron")}
    o_links = arms["O"]["links"] or 1
    bar = {"right_header_links_over_offered_O": {k: None if v is None else round(v, 3) for k, v in o_rate.items()},
           "enron_served_unjudged_logged_vs_O": enron_u,
           "wrong_share_of_links_O": round(arms["O"]["wrong"] / o_links, 3),
           "passed_strict": bool(o_rate["strict"] is not None and o_rate["strict"] >= .5
                                 and enron_u["enron"][1] <= enron_u["enron"][0] / 2
                                 and arms["O"]["wrong"] <= .10 * o_links)}
    fit_o = {s: [round(m, 3), round(u, 3)] for s, (m, u) in sorted(company_final["O"][1].items())}
    fit_logged = {s: [round(m, 3), round(u, 3)] for s, (m, u) in sorted(company_final["logged"][1].items())}
    return {"gates": gates, "decider": tally,
            "units": {k: per_unit(units[k]) for k in ("strict", "partly", "enron", "partly_enron")},
            "enron_landing": {k: {f"{a}: {b}": n for (a, b), n in sorted(v.items())} for k, v in enron_landing.items()},
            "links_judged": {a: dict(c) for a, c in arms.items()},
            "served_counterparties": {" / ".join(map(str, k)): n for k, n in sorted(served.items(), key=str)},
            "company_fit_end": {"logged": {"prior": round(company_final["logged"][0], 3), "m_u": fit_logged},
                                "O": {"prior": round(company_final["O"][0], 3), "m_u": fit_o}},
            "bar_O": bar}


def main():
    run = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "runs/party/ours/ward-party"
    out = read(run)
    print(json.dumps(out, indent=1))
    (P.DATA / "party.json").write_text(json.dumps(out, indent=1) + "\n")


if __name__ == "__main__":
    main()
