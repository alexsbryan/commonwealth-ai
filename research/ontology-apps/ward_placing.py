"""Ward's PLACE rung, read a second time: where does each gold deal's counterparty live? (order ontology-layer-12)

    ward_placing.py [RUN] [--json out.json]

Every tune gold deal ladder_ward.py counts (its items, its Facts) gets one row: per message of the deal, whether the
gold counterparty is among the message's header-derived outside parties (the recipe's `outside_parties` path,
`document / (from | to | cc) / employer [!ours]`, and `party_of_message` = the first of them), whether the text the
run indexed for that message names it, the `party` each stage_update claim on it carries, and what RESOLVE decided
for those claims (resolve_decisions.jsonl and the recorded log). A PLACE-lost deal is classed by CAUSES, a closed list
committed before any row was read; the rest are classed by their ladder stage, so the classes sum to the ladder.

"Names it", case-insensitive: one of gold's written forms (ward/score.py gold_forms: the name and each parenthetical
alias, through name_core) as a whole-word run of the folded text, or one of gold's domains as a substring of the
lowercased text. "Outside party": an address in From, To or Cc whose domain does not end in enron.com (the `ours`
set), equal to a gold domain or a subdomain of one.
"""
import enum


class Cause(enum.Enum):
    """Why a PLACE-lost deal's latest claim sits on no record matched to it. Decided on the deal's latest messages
    (gold's latest stage_update files, the ones ladder_ward's PLACE fact reads), in this order:

    NO_JOIN        gold's counterparty joins no company atom of the run (by a gold domain): the order's stop condition
    DERIVATION     gold's counterparty IS an outside party of a latest message's headers, yet no stage_update claim on
                   the latest messages carries a `party` resolving to it (nothing derived, or another company first)
    NO_LINK        gold's counterparty is an outside party of a latest message's headers AND a stage_update claim on
                   the latest messages carries it as `party`: the party was there and nothing linked by it
    BODY_ONLY      not an outside party of any latest message's headers, but the indexed text of one names it
    ABSENT         neither in the latest messages' headers nor in their indexed text
    OTHER          anything else (e.g. a claim carries it as `party` though no latest header shows it)
    """
    NO_JOIN = "gold counterparty joins no company atom"
    DERIVATION = "party derivation wrong"
    NO_LINK = "party in headers but no linking source"
    BODY_ONLY = "party only in body"
    ABSENT = "party absent"
    OTHER = "other"


# What each cause points at (named with the counts; the seat picks): domain-free changes only.
POINTS_AT = {
    Cause.NO_JOIN: "the instrument or the company source, not placing",
    Cause.DERIVATION: "the party derivation (the `document` step / `first` fold), before any RESOLVE change",
    Cause.NO_LINK: "declared derived roles (stage_update.party) as a RESOLVE identity source",
    Cause.BODY_ONLY: "the unbuilt Pick pass: a reference read from text among declared candidates",
    Cause.ABSENT: "neither: the deal's latest messages do not carry its party",
    Cause.OTHER: "read case by case",
}


def classify(joined, in_headers, party_claimed, in_body):
    """One PLACE-lost deal's cause from four facts on its latest messages: `joined` (gold counterparty has a company
    atom), `in_headers` (it is an outside party of some latest message), `party_claimed` (some stage_update claim on
    a latest message carries a `party` resolving to it), `in_body` (some latest message's indexed text names it)."""
    if not joined:
        return Cause.NO_JOIN
    if in_headers:
        return Cause.NO_LINK if party_claimed else Cause.DERIVATION
    if party_claimed:
        return Cause.OTHER
    return Cause.BODY_ONLY if in_body else Cause.ABSENT


# ---------------------------------------------------------------- the join (step 1): run, gold and the recorded logs
import argparse, collections, email.utils, json, pathlib, re, sys  # noqa: E402

import ladder as L  # noqa: E402
import ladder_ward as W  # noqa: E402

S = W.S
OURS = "enron.com"  # the recipe's `ours` set: domain suffix enron.com
ADDRESS_LINE = re.compile(r"^(from|to|cc|bcc|date):", re.I)  # the reader's header lines: the address fields, not body
BECOMES = re.compile(r'record becomes an atom type=deal record=(\S+) atom=(\S+)')
DERIVE = re.compile(r"DEBUG atlas/derive: (.*?) (?:claim|atom)=(claim-\d+)(.*)$")


def domain_of(addr):
    return addr.rsplit("@", 1)[-1].strip().lower() if "@" in addr else None


def outside_parties(meta, companies):
    """The recipe's `outside_parties` on one message, in order: from, to, cc addresses' domains that reach a company
    atom of the run (`employer`, the domain reader: a domain with no company atom, e.g. a mail provider, reaches
    nothing), `ours` dropped."""
    out = []
    for field in ("from", "to", "cc"):
        for _, addr in email.utils.getaddresses([meta.get(field) or ""]):
            d = domain_of(addr)
            if d and d in companies and not (d == OURS or d.endswith("." + OURS)) and d not in out:
                out.append(d)
    return out


def is_gold(dom, gold_domains):
    return any(dom == g or dom.endswith("." + g) for g in gold_domains)


def names(text, forms, gold_domains):
    """Which of gold's forms / domains the text names (the module docstring's rule)."""
    folded, low = f" {S.fold(text)} ", text.lower()
    hit = [f for f in forms if len(f) >= 2 and re.search(rf"(?<![a-z0-9]){re.escape(f)}(?![a-z0-9])", folded)]
    return hit + [g for g in gold_domains if g in low]


def messages(index):
    """message file -> {"meta": the chunk metadata (from/to/cc as the run's source read them), "text": the indexed
    text, address lines removed}, through the run's own chunks.lance and the manifest."""
    import lance  # noqa: PLC0415
    path_of = {m: e["path"] for e in json.loads((S.WARD / "manifest.json").read_text()) for m in e["message_ids"]}
    out = {}
    rows = lance.dataset(str(index / "chunks.lance")).to_table(columns=["content", "metadata"]).to_pylist()
    for c in rows:
        meta = json.loads(c["metadata"] or "{}")
        f = path_of.get(meta.get("message_id") or "")
        if not f:
            continue
        m = out.setdefault(f, {"meta": meta, "text": []})
        m["text"].append("\n".join(l for l in (c["content"] or "").splitlines() if not ADDRESS_LINE.match(l.strip())))
    return {f: {"meta": m["meta"], "text": "\n".join(m["text"])} for f, m in out.items()}


def resolve_trace(run, atlas_dir):
    """What RESOLVE did, from the recorded half: atom -> record, statement -> outcome, document -> its decision row,
    claim -> the derive lines naming it."""
    lines, _ = L.recorded_half(run / "job.debug.log")
    atom_record, derive = {}, collections.defaultdict(list)
    for raw in lines:
        m = BECOMES.search(raw)
        if m:
            atom_record[m.group(2)] = m.group(1)
            continue
        d = DERIVE.search(raw.rstrip())
        if d:
            derive[d.group(2)].append(f"{d.group(1)}{d.group(3)}".strip())
    docs = [json.loads(l) for l in (atlas_dir / "resolve_decisions.jsonl").read_text().splitlines() if l.strip()]
    statements = {o["statement"]: dict(o, document=x["document"]) for x in docs for o in x["outcomes"]}
    return atom_record, statements, {x["document"]: x for x in docs}, derive


def load(run):
    """Everything the rows read, through ladder_ward's own population (measure's items) and loaders."""
    run = pathlib.Path(run).resolve()
    lad = W.measure(run)
    if lad.get("status") != "judged":
        return lad
    found = L.run_atlas(run)
    secfiles = S.section_files(str(found["index"]))
    view = L.atlas_view(run, found)
    ent, claims = S.load_atlas(view["atoms"], secfiles, view["decisions"])
    g = S.load_gold(S.WARD / "gold")
    tune = {f for f in g["files"] if f.split("/", 1)[0] in W.TUNE}
    res = S.score(g, ent, claims, tune)  # the population's own matches; measure's items() reads the same
    return {"run": run, "found": found, "ladder": lad, "ent": ent, "claims": claims, "g": g, "res": res,
            "msgs": messages(found["index"]), "trace": resolve_trace(run, found["dir"]), "decisions": view["decisions"],
            "company_domains": {S.fold(d) for e in ent.values() if e.get("entity_type") == "company"
                                for d in S.attr_list(e, "domain")}}


def deal_row(item, d, X, atoms_of, company_resolves):
    ent, claims, msgs = X["ent"], X["claims"], X["msgs"]
    atom_record, statements, docs, derive = X["trace"]
    party_decision = {x["atom"]: x for x in X["decisions"] if x.get("attribute") == "party"}
    cp = next((c for c in X["g"]["companies"] if c["id"] == d["counterparty"]), None)
    gold_domains = [S.fold(x) for x in (cp or {}).get("domains") or []]
    forms = sorted(S.gold_forms(cp)) if cp else []
    latest = set(item["latest"])
    subject = {**X["res"]["deal_matches"]["transactions"], **X["res"]["deal_matches"]["master_agreements"]}.get(d["id"])
    per_msg = []
    for f in sorted(d["files"], key=S.when):
        m = msgs.get(f)
        outside = outside_parties(m["meta"], X["company_domains"]) if m else None
        su = [c for c in claims if c.get("claim_kind") == "stage_update" and f in c["_files"]]
        cl = []
        for c in su:
            party = (c.get("attributes") or {}).get("party")
            rec = atom_record.get(c.get("subject"))
            outs = [dict(statement=k, decision=o["outcome"].get("decided", {}).get("decision"),
                         choice=o.get("choice"), sources=o["outcome"].get("decided", {}).get("sources"))
                    for k, o in statements.items() if o["outcome"].get("decided", {}).get("record") == rec
                    and k.split("@", 1)[0] in {e.get("source_doc_id") for e in c.get("evidence") or []}]
            pd = party_decision.get(c["id"], {})
            cl.append({"claim": c["id"], "stage": (c.get("attributes") or {}).get("stage"), "subject": c.get("subject"),
                       "on_matched_record": subject is not None and c.get("subject") == subject,
                       "party": party, "party_domain": (ent.get(party) or {}).get("attributes", {}).get("domain") if party else None,
                       "party_is_gold": bool(party) and company_resolves(party, d["counterparty"]),
                       "party_derivation": {"outcome": pd.get("outcome"), "documents": pd.get("documents"),
                                            "log": derive.get(c["id"], [])},
                       "record": rec, "resolve": outs})
        text = m["text"] if m else ""
        doc = (m or {}).get("meta", {}).get("message_id", "")
        did = W.doc_id(doc) if doc else None
        per_msg.append({"file": f, "latest": f in latest, "indexed": m is not None, "outside_parties": outside,
                        "in_headers": None if outside is None else any(is_gold(o, gold_domains) for o in outside),
                        "first_is_gold": None if not outside else is_gold(outside[0], gold_domains),
                        "body_names": names(text, forms, gold_domains) if m else None,
                        "resolve_document": {k: (docs.get(did) or {}).get(k) for k in ("candidates", "calls", "unread", "necessary")}
                        if did in docs else None,
                        "stage_updates": cl})
    L_ = [m for m in per_msg if m["latest"]]
    facts = {"joined": d["counterparty"] in atoms_of,
             "in_headers": any(m["in_headers"] for m in L_),
             "party_claimed": any(c["party_is_gold"] for m in L_ for c in m["stage_updates"]),
             "in_body": any(m["body_names"] for m in L_)}
    stage = item["ladder_stage"]
    cls = classify(**facts).name if stage == "place" else stage
    return {"deal": d["id"], "kind": d.get("kind", "transaction"), "counterparty": (cp or {}).get("name"),
            "gold_domains": gold_domains, "forms": forms, "company_atoms": sorted(atoms_of.get(d["counterparty"], ())),
            "ladder_stage": stage, "rung": item["rung"], "record": item["record"], "matched_record": subject,
            "class": cls, "facts": facts,
            "any_message": {"in_headers": any(m["in_headers"] for m in per_msg),
                            "in_body": any(m["body_names"] for m in per_msg),
                            "party_claimed": any(c["party_is_gold"] for m in per_msg for c in m["stage_updates"])},
            "messages": per_msg}


def place_detail(rows):
    """Per cause, beside its count: the rungs, why a NO_JOIN joins nothing (gold names no domain, or its domains are
    in no header of the run: the company source reads every document's from/to/cc), what the latest messages'
    headers did carry as outside parties and as each claim's `party`, and RESOLVE's decisions on those claims."""
    out = {}
    for r in rows:
        lt = [m for m in r["messages"] if m["latest"]]
        cl = [c for m in lt for c in m["stage_updates"]]
        o = out.setdefault(r["class"], {"n": 0, "deals": [], "rungs": collections.Counter(),
                                        "named_in_latest_body": 0, "party_in_any_message_headers": 0,
                                        "no_join_why": collections.Counter(), "latest_outside_parties": collections.Counter(),
                                        "claim_party": collections.Counter(), "resolve_decisions": collections.Counter()})
        o["n"] += 1
        o["deals"].append(r["deal"])
        o["rungs"][r["rung"]] += 1
        o["named_in_latest_body"] += r["facts"]["in_body"]
        o["party_in_any_message_headers"] += r["any_message"]["in_headers"]
        if r["class"] == Cause.NO_JOIN.name:
            o["no_join_why"]["gold names no domain" if not r["gold_domains"] else "gold's domains in no header"] += 1
        for m in lt:
            o["latest_outside_parties"][",".join(m["outside_parties"] or []) or "none"] += 1
        for c in cl:
            o["claim_party"][c["party_domain"] or f"none ({c['party_derivation']['outcome']})"] += 1
            for x in c["resolve"]:
                o["resolve_decisions"][x["decision"]] += 1
    return {k: {kk: (dict(vv) if isinstance(vv, collections.Counter) else vv) for kk, vv in v.items()} for k, v in out.items()}


def validate(X):
    """The instrument first: for each stage_update claim whose party the run derived from one document, the first
    outside party this module derives from that document's headers must be the derived party's domain."""
    path_of = {m["meta"].get("message_id"): f for f, m in X["msgs"].items()}
    by_doc = {W.doc_id(k): f for k, f in path_of.items() if k}
    agree, disagree, rows = 0, 0, []
    for x in X["decisions"]:
        if x.get("attribute") != "party" or x.get("outcome") != "decided" or len(x.get("documents") or []) != 1:
            continue
        f = by_doc.get(x["documents"][0])
        mine = outside_parties(X["msgs"][f]["meta"], X["company_domains"]) if f else None
        theirs = [(X["ent"].get(v) or {}).get("attributes", {}).get("domain") for v in x.get("values") or []]
        ok = bool(mine) and theirs == [mine[0]]
        agree += ok
        disagree += not ok
        if not ok:
            rows.append({"claim": x["atom"], "file": f, "mine": mine, "derived": theirs})
    return {"agree": agree, "disagree": disagree, "disagreements": rows}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("run", nargs="?", default=str(HERE_RUN))
    ap.add_argument("--json", type=pathlib.Path)
    a = ap.parse_args()
    X = load(a.run)
    if "ladder" not in X:
        print(json.dumps(X))
        return 4
    lad = X["ladder"]
    atoms_of, company_resolves = S.company_resolver(X["g"], X["ent"])
    deals = {d["id"]: d for d in X["g"]["deals"]}
    rows = [deal_row(i, deals[i["deal"]], X, atoms_of, company_resolves) for i in lad["items"]]
    by_stage = collections.Counter(r["ladder_stage"] for r in rows)
    want = {s: lad["ladder"]["stages"][s]["lost"] for s in ("read", "place", "fold")} | {"hit": lad["ladder"]["hit"]}
    sums = {"rows": len(rows), "n": lad["ladder"]["n"], "by_stage": dict(by_stage), "ladder": want,
            "ok": len(rows) == lad["ladder"]["n"] and all(by_stage.get(k, 0) == v for k, v in want.items())}
    classes = collections.Counter(r["class"] for r in rows)
    out = {"run": str(X["run"]), "read": lad["read"], "instrument": validate(X), "sums": sums,
           "classes": dict(classes),
           "causes": {c.name: {"label": c.value, "n": classes.get(c.name, 0), "points_at": POINTS_AT[c]} for c in Cause},
           "facts_by_stage": {s: dict(collections.Counter(
               "/".join(k for k, v in r["facts"].items() if v) or "none" for r in rows if r["ladder_stage"] == s))
               for s in by_stage},
           "place_lost": place_detail([r for r in rows if r["ladder_stage"] == "place"]),
           "rows": rows}
    text = json.dumps(out, indent=1, default=sorted)
    if a.json:
        a.json.write_text(text + "\n")
    print(json.dumps({k: out[k] for k in ("instrument", "sums", "classes", "facts_by_stage", "place_lost")}, indent=1, default=sorted))
    return 0 if sums["ok"] else 1


HERE_RUN = pathlib.Path(__file__).resolve().parents[2] / "runs/e4-locate-values/ward-tune"

if __name__ == "__main__":
    sys.exit(main())
