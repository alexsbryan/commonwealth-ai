#!/usr/bin/env python3
"""crm-proof scorer: the crm-ward atlas's typed records against the Ward gold (GOLD_SPEC v1).

    score.py [--corpus crm-ward] [--json out.json]     # score
    score.py --selftest                                 # the instrument on a hand case first
    score.py --bar crm-people                           # one bar's holdout value, co-lineage's contract

No judge model. A pipeline record meets a gold record only through the evidence it cites:
atoms cite SECTIONS (the email extractor keys a section by Subject, so one section can hold
several messages); a claim's verbatim anchor narrows its section to the message file that
contains it, and an entity's files are its first appearance plus every claim made about it.

Deal matching uses the existing evidence-plus-counterparty edge predicate and deterministic
maximum-cardinality matching. It measures evidence coverage, not descriptor-level transaction
identity proof. Gold denominators are selected by gold source files, independent of extraction
coverage; coverage remains a separate diagnostic.

Per bar (quality/campaigns/crm-proof.toml), read on the holdout (files outside the smoke sections)
with the full set beside:
  people       gold people with an email held by exactly ONE person atom carrying that email
  companies    gold companies matched by a domain or the folded name
  contacts     external gold people whose person atom's employer resolves to their gold company
  deals        gold transaction deals matched by a deal atom citing one of their messages whose
                counterparty resolves to the gold counterparty; each atom used once
  stage_served gold deals with a current stage (their latest gold update, by message date) whose
                matched transaction or master-agreement atom SERVES that stage (the recipe's
                `deal_stage` fold, read off the atom), and every document the fold decided on
                (atlas/derived_decisions.jsonl) is a message gold places that deal at that stage;
                each miss is counted by why beside it. The instrument never folds a stage itself
  commitments  gold commitments matched by a commitment claim citing the file whose subject is
               the gold person's atom
Reported beside: atoms matching nothing, emails split across atoms, made-up claims (a claim on
a file where gold has none of that kind), and `stage`: gold stage_updates matched by a
stage_update claim citing the file, subject = the matched atom, same stage (the reading
diagnostic, crm-stage's bar under ward-score-v2).
"""
import argparse, email.utils, json, pathlib, re, sys, unicodedata

HERE = pathlib.Path(__file__).resolve().parent
HOME = pathlib.Path.home()
WARD = HOME / ".svrnmesh/bench-corpora/enron-ward"
INSTRUMENT_VERSION = "ward-score-v3"


def fold(s):
    s = unicodedata.normalize("NFKD", str(s or "")).encode("ascii", "ignore").decode().lower()
    return " ".join(re.sub(r"[^a-z0-9@.]+", " ", s).split())


def squash(s):
    return " ".join(str(s or "").split()).lower()


DATES = {}  # folder/file -> sortable date, from the manifest; unknown dates sort by file name
DOC_FILES = {}  # the index's source_doc_id -> folder/file, so a fold's provenance documents read as gold's files


def when(f):
    return DATES.get(f) or f


def section_files(corpus):
    """sec id -> [(folder/file, folded body)] via chapters.json chunk ids -> chunk Message-ID -> manifest."""
    import lance  # noqa: PLC0415  (only the scoring path needs it)
    idx = HOME / ".svrnmesh/indexes" / corpus
    chunks = lance.dataset(str(idx / "chunks.lance")).to_table(columns=["id", "metadata", "source_doc_id"]).to_pylist()
    mid_of = {str(c["id"]): (json.loads(c["metadata"] or "{}").get("message_id") or "") for c in chunks}
    manifest = json.loads((WARD / "manifest.json").read_text())
    path_of = {m: e["path"] for e in manifest for m in e["message_ids"]}
    for c in chunks:
        if c.get("source_doc_id") and mid_of[str(c["id"])] in path_of:
            DOC_FILES[c["source_doc_id"]] = path_of[mid_of[str(c["id"])]]
    for e in manifest:
        try:
            DATES[e["path"]] = email.utils.parsedate_to_datetime(e["date"]).isoformat()
        except (TypeError, ValueError):
            pass
    out = {}
    for ch in json.loads((idx / "chapters.json").read_text())["chapters"]:
        files = sorted({path_of[mid_of[str(i)]] for i in ch["chunk_ids"] if mid_of.get(str(i)) in path_of})
        out[ch["id"]] = [(f, squash((WARD / "sample" / f).read_text(errors="replace"))) for f in files]
    return out


def load_gold(gold_dir):
    g = {"people": [], "companies": [], "deals": [], "stage_updates": [], "commitments": [], "files": set()}
    for p in sorted(gold_dir.glob("*.json")):
        d = json.loads(p.read_text())
        f = d["folder"]
        q = lambda i: f"{f}:{i}"  # noqa: E731  (ids are local to a folder)
        source_files = d.get("files_read", []) + d.get("files_with_nothing", [])
        g["files"] |= {f"{f}/{x}" for x in source_files}
        g["people"] += [dict(x, id=q(x["id"]), company=q(x.get("company"))) for x in d["people"]]
        g["companies"] += [dict(x, id=q(x["id"])) for x in d["companies"]]
        g["deals"] += [dict(x, counterparty=q(x["counterparty"]), files={f"{f}/{m}" for m in x["messages"]})
                       for x in d["deals"]]
        g["stage_updates"] += [dict(x, file=f"{f}/{x['file']}") for x in d["stage_updates"]]
        g["commitments"] += [dict(x, person=q(x["person"]), file=f"{f}/{x['file']}") for x in d["commitments"]]
    return g


def load_decisions(path):
    """The build's derived_decisions.jsonl beside an atlas; absent is no decisions, which every served stage misses on."""
    p = pathlib.Path(path)
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []


def load_atlas(atoms, secfiles, decisions=()):
    ent = {x["data"]["id"]: x["data"] for x in atoms if x["atom_type"] == "Entity"}
    claims = [x["data"] for x in atoms if x["atom_type"] == "Claim"]
    for d in decisions:  # a deal's stage decision: its outcome and the files of the documents it decided on
        if d.get("attribute") == "stage" and (ent.get(d.get("atom")) or {}).get("entity_type") == "deal":
            ent[d["atom"]]["_stage"] = {"outcome": d.get("outcome"), "values": d.get("values") or [],
                                        "files": [DOC_FILES.get(k) for k in d.get("documents") or []]}

    def files(sec, anchor=None):
        cand = secfiles.get(sec, [])
        if len(cand) == 1:
            return {cand[0][0]}
        if len(cand) > 1:
            normalized = squash(anchor)
            if not normalized:
                return set()
            # A repeated passage is still ambiguous; assigning every hit would guess.
            hit = [f for f, body in cand if normalized in body]
            return {hit[0]} if len(hit) == 1 else set()
        return set()

    for c in claims:
        c["_files"] = set().union(set(), *(files(e.get("chunk_id"), c.get("anchor")) for e in c.get("evidence") or []))
    for e in ent.values():
        fa = (e.get("first_appearance") or {}).get("chunk_id")
        e["_files"] = files(fa, (e.get("first_appearance") or {}).get("passage_preview")) if fa else set()
    for c in claims:
        if c.get("subject") in ent:
            ent[c["subject"]]["_files"] |= c["_files"]
    return ent, claims


def attr_list(e, k):
    v = (e.get("attributes") or {}).get(k)
    return [x for x in (v if isinstance(v, list) else [v]) if x]


# The scorer's own name normalisation, kept apart from the system's (compose.py) so a bug in one cannot hide
# in the other: legal forms, a trailing state code and a leading article do not change which company it is.
LEGAL_FORMS = {"inc", "incorporated", "corp", "corporation", "co", "company", "llc", "llp", "lp", "ltd", "limited", "plc"}


def name_core(n):
    n = re.sub(r"\(.*?\)", " ", n or "")
    n = re.sub(r",\s*[A-Z]{2}\.?\s*$", " ", n.strip())
    words = [w for w in fold(n).replace(".", " ").replace("@", " ").split() if w not in LEGAL_FORMS]  # fold keeps them for addresses
    return " ".join(words[1:] if words[:1] == ["the"] else words)


def gold_forms(c):
    """A gold company's written forms: its name, and each alias its name gives in parentheses."""
    inner = [x for par in re.findall(r"\(([^)]*)\)", c.get("name") or "") for x in re.split(r"[;/]", par)]
    return {name_core(x) for x in [c.get("name")] + inner} - {""}


def atom_forms(e):
    """The atom's canonical name only: its aliases are the system's own identity claims (an over-merge writes the
    absorbed name there), and an instrument never matches on what the subject supplies about itself."""
    return {name_core(e.get("canonical_name"))} - {""}


def company_resolver(g, ent):
    """The one rule matching a gold company to the atlas's company atoms (every bar and deals.py read it):
    a shared domain, else a shared written form (gold_forms against the atom's canonical name, equal after
    name_core; never a subset, so "Citizens" is not "Citizens Insurance"). -> (atoms_of: gold id -> atom ids,
    resolves(atom value, gold id)), where an atom value is an atom id or a raw name."""
    companies = [e for e in ent.values() if e.get("entity_type") == "company"]
    atoms_of, forms_of = {}, {c["id"]: gold_forms(c) for c in g["companies"]}
    for c in g["companies"]:
        doms = {fold(d) for d in c.get("domains") or []}
        hit = {e["id"] for e in companies if doms & {fold(d) for d in attr_list(e, "domain")} or forms_of[c["id"]] & atom_forms(e)}
        if hit:
            atoms_of[c["id"]] = hit

    def resolves(v, gold_id):
        if v in atoms_of.get(gold_id, set()):
            return True
        return bool(forms_of.get(gold_id, set()) & (atom_forms(ent[v]) if v in ent else {name_core(v)}))
    return atoms_of, resolves


def maximum_matching(left, right, has_edge):
    """Return a deterministic maximum-cardinality left-id -> right-id assignment."""
    ordered_left = sorted(left, key=lambda x: str(x["id"]))
    ordered_right = sorted(right, key=lambda x: str(x["id"]))
    adjacency = {
        item["id"]: [candidate["id"] for candidate in ordered_right if has_edge(item, candidate)]
        for item in ordered_left
    }
    owner = {}

    def augment(left_id, seen_right):
        for right_id in adjacency[left_id]:
            if right_id in seen_right:
                continue
            seen_right.add(right_id)
            if right_id not in owner or augment(owner[right_id], seen_right):
                owner[right_id] = left_id
                return True
        return False

    for item in ordered_left:
        augment(item["id"], set())
    assigned = {left_id: right_id for right_id, left_id in owner.items()}
    return {item["id"]: assigned[item["id"]] for item in ordered_left if item["id"] in assigned}


def scoring_scopes(gold_files, smoke_files):
    """Choose folds from the gold file inventory; extraction coverage never defines eligibility."""
    all_files = set(gold_files)
    dev_files = all_files & set(smoke_files)
    return {"all": all_files, "holdout": all_files - dev_files, "dev": dev_files}


def score(g, ent, claims, scope):
    """scope: the set of message files counted (gold labels outside it are not scored)."""
    of = lambda t: [e for e in ent.values() if e.get("entity_type") == t]  # noqa: E731
    persons, companies, deals = of("person"), of("company"), of("deal")
    holders = {}
    for e in persons:
        for m in attr_list(e, "email"):
            holders.setdefault(m.lower(), set()).add(e["id"])
    gp = [p for p in g["people"] if p.get("emails") and any(m.lower() for m in p["emails"])]
    person_atom, split, ph = {}, 0, 0
    for p in gp:
        ids = set().union(set(), *(holders.get(m.lower(), set()) for m in p["emails"]))
        if len(ids) == 1:
            ph += 1; person_atom[p["id"]] = next(iter(ids))
        split += len(ids) > 1
    for p in g["people"]:  # name fallback for commitments only
        if p["id"] not in person_atom:
            hit = [e["id"] for e in persons if fold(e.get("canonical_name")) == fold(p.get("name"))]
            if len(hit) == 1:
                person_atom.setdefault(p["id"] + "#name", hit[0])
    company_atom, company_resolves = company_resolver(g, ent)

    def resolves(deal_atom, gold_cp):
        return any(company_resolves(v, gold_cp) for v in attr_list(deal_atom, "counterparty"))

    # Contact -> Account: an external gold person (an email and a company) whose one person atom names an employer
    # that resolves to that company; an employer resolving to no gold company of theirs is a wrong account
    contact_n = contact_hit = contact_wrong = 0
    gold_companies = {c["id"] for c in g["companies"]}
    for p in gp:
        if p.get("internal") or p.get("company") not in gold_companies:  # gold's "None" loads as "<folder>:None"
            continue
        contact_n += 1
        emp = attr_list(ent[person_atom[p["id"]]], "employer") if p["id"] in person_atom else []
        ok = any(company_resolves(v, p["company"]) for v in emp)
        contact_hit += ok
        contact_wrong += bool(emp) and not ok
    eligible_deals = [d for d in g["deals"] if d["files"] & scope]
    gd = [d for d in eligible_deals if d.get("kind", "transaction") == "transaction"]
    gm = [d for d in eligible_deals if d.get("kind") == "master_agreement"]

    def evidence_party_edge(d, e):
        return bool(e["_files"] & d["files"]) and resolves(e, d["counterparty"])

    # The transaction population owns first choice of atoms so adding master agreements
    # cannot reduce the pre-existing transaction-deal bar.
    deal_atom = maximum_matching(gd, deals, evidence_party_edge)
    used = set(deal_atom.values())
    master_atom = maximum_matching(gm, [e for e in deals if e["id"] not in used], evidence_party_edge)
    stage_subject = {**deal_atom, **master_atom}
    gs = [s for s in g["stage_updates"] if s["file"] in scope]
    sh = sum(1 for s in gs if any(c.get("claim_kind") == "stage_update" and s["file"] in c["_files"]
                                  and stage_subject.get(s["deal"]) is not None
                                  and c.get("subject") == stage_subject[s["deal"]]
                                  and (c.get("attributes") or {}).get("stage") == s["stage"] for c in claims))
    updates_by_deal = {}
    for update in g["stage_updates"]:
        updates_by_deal.setdefault(update["deal"], []).append(update)
    current_deals, gold_current = [], {}
    for d in eligible_deals:
        updates = updates_by_deal.get(d["id"], [])
        if not updates:
            continue
        latest_when = max(when(s["file"]) for s in updates)
        latest_updates = [s for s in updates if when(s["file"]) == latest_when]
        latest_stages = {s["stage"] for s in latest_updates}
        if len(latest_stages) == 1 and None not in latest_stages:
            current_deals.append(d)
            gold_current[d["id"]] = next(iter(latest_stages))
    # The CRM shows a deal's current stage with the message it came from: credit the stage the matched atom SERVES
    # only when every document its decision names is one gold places that deal at that stage. Each miss says why.
    # unbacked: no decided decision names the served value; provenance_unmapped: a deciding document the index
    # cannot name as a gold file (the instrument could not judge it, apart from a message gold does not place)
    served_hit, missed = 0, dict.fromkeys(("unmatched_deal", "wrong_stage", "unbacked", "unplaced",
                                           "provenance_unmapped", "no_decision"), 0)
    for d in current_deals:
        subject = stage_subject.get(d["id"])
        if subject is None:
            missed["unmatched_deal"] += 1
            continue
        served = (ent[subject].get("attributes") or {}).get("stage")
        decision = ent[subject].get("_stage")
        if served is None:
            why = decision["outcome"] if decision else "no_decision"
            missed[why] = missed.get(why, 0) + 1
            continue
        if served != gold_current[d["id"]]:
            missed["wrong_stage"] += 1
            continue
        placed = {s["file"] for s in updates_by_deal[d["id"]] if s["stage"] == served}
        if not decision:
            missed["no_decision"] += 1
        elif decision["outcome"] != "decided" or decision["values"] != [served] or not decision["files"]:
            missed["unbacked"] += 1
        elif None in decision["files"]:
            missed["provenance_unmapped"] += 1
        elif set(decision["files"]) <= placed:
            served_hit += 1
        else:
            missed["unplaced"] += 1
    gc = [c for c in g["commitments"] if c["file"] in scope]
    ch = sum(1 for k in gc if any(c.get("claim_kind") == "commitment" and k["file"] in c["_files"]
                                  and c.get("subject") in {person_atom.get(k["person"]), person_atom.get(k["person"] + "#name")} - {None}
                                  for c in claims))
    labelled = lambda kind: {x["file"] for x in g[kind]}  # noqa: E731
    made_up = {k: sum(1 for c in claims if c.get("claim_kind") == ck and c["_files"] & scope
                      and not c["_files"] & labelled(k)) for k, ck in (("stage_updates", "stage_update"), ("commitments", "commitment"))}
    r = lambda h, n: {"hit": h, "n": n, "recall": round(h / n, 3) if n else None}  # noqa: E731
    # people and companies carry no file in the gold, so they are not scoped: read them on a
    # build that covers every gold file
    return {"people": r(ph, len(gp)), "emails_split": split,
            "contacts": r(contact_hit, contact_n), "contacts_wrong_account": contact_wrong,
            "companies": r(len(company_atom), len(g["companies"])),
            "deals": r(len(deal_atom), len(gd)), "master_agreements": r(len(master_atom), len(gm)),
            "deal_atoms_unmatched": len(deals) - len(used),
            "deal_matches": {"transactions": deal_atom, "master_agreements": master_atom},
            "stage_served": r(served_hit, len(current_deals)), "stage_served_missed": missed, "stage": r(sh, len(gs)),
            "commitments": r(ch, len(gc)), "made_up": made_up,
            "person_atoms_unmatched": sum(1 for e in persons if e["id"] not in set(person_atom.values()))}


def selftest():
    """A hand case with a known answer per bar, including each goodhart path."""
    sec = {"s1": [("f/1", squash("Price is $4.12 for five years. I will send the confirm Friday."))],
           "s2": [("f/2", squash("We accept the deal.")), ("f/3", squash("unrelated newsletter"))]}
    g = {"files": {"f/1", "f/2", "f/3"},
         "people": [{"id": "f:p1", "name": "Ann Lee", "emails": ["ann@city.gov"], "company": "f:c1"},
                    {"id": "f:p2", "name": "Bo Ray", "emails": ["bo@x.com"], "company": "f:c1"}],
         "companies": [{"id": "f:c1", "name": "City of X", "domains": ["city.gov"]}],
         "deals": [{"id": "f-d1", "counterparty": "f:c1", "kind": "transaction", "files": {"f/1", "f/2"}},
                   {"id": "f-d2", "counterparty": "f:c1", "kind": "transaction", "files": {"f/3"}}],
         "stage_updates": [{"deal": "f-d1", "stage": "proposal", "file": "f/1"},
                           {"deal": "f-d1", "stage": "won", "file": "f/2"},
                           {"deal": "f-d2", "stage": "lead", "file": "f/3"}],
         "commitments": [{"person": "f:p1", "file": "f/1"}, {"person": "f:p2", "file": "f/2"}]}
    atoms = [{"atom_type": "Entity", "data": {"id": "e7", "entity_type": "company", "canonical_name": "Other Co",
                                              "attributes": {"domain": "other.com"}}},
             # a decoy deal citing the same message with the WRONG counterparty, listed first
             {"atom_type": "Entity", "data": {"id": "e0", "entity_type": "deal", "canonical_name": "other deal",
                                              "first_appearance": {"chunk_id": "s1"}, "attributes": {"counterparty": "e7"}}},
             # the right stage on the right message, but about the decoy deal
             {"atom_type": "Claim", "data": {"id": "k5", "claim_kind": "stage_update", "subject": "e0", "anchor": "We accept",
                                             "evidence": [{"chunk_id": "s2"}], "attributes": {"stage": "won"}}},
             {"atom_type": "Entity", "data": {"id": "e1", "entity_type": "person", "canonical_name": "Ann Lee",
                                              "attributes": {"email": "ann@city.gov"}}},
             {"atom_type": "Entity", "data": {"id": "e2", "entity_type": "person", "canonical_name": "Bo",
                                              "attributes": {"email": "bo@x.com"}}},
             {"atom_type": "Entity", "data": {"id": "e3", "entity_type": "person", "canonical_name": "Bo R",
                                              "attributes": {"email": "BO@x.com"}}},
             {"atom_type": "Entity", "data": {"id": "e4", "entity_type": "company", "canonical_name": "City of X",
                                              "attributes": {"domain": "city.gov"}}},
             {"atom_type": "Entity", "data": {"id": "e5", "entity_type": "deal", "canonical_name": "5-yr supply",
                                              "first_appearance": {"chunk_id": "s1"},
                                              "attributes": {"counterparty": "e4", "stage": "won"}}},
             {"atom_type": "Claim", "data": {"id": "k1", "claim_kind": "stage_update", "subject": "e5", "anchor": "Price is $4.12",
                                             "evidence": [{"chunk_id": "s1"}], "attributes": {"stage": "proposal"}}},
             {"atom_type": "Claim", "data": {"id": "k2", "claim_kind": "stage_update", "subject": "e5", "anchor": "We accept",
                                             "evidence": [{"chunk_id": "s2"}], "attributes": {"stage": "negotiating"}}},
             {"atom_type": "Claim", "data": {"id": "k3", "claim_kind": "commitment", "subject": "e1", "anchor": "I will send",
                                             "evidence": [{"chunk_id": "s1"}]}},
             # a commitment on f/2 attributed to the wrong person (gold's is Bo's, whose email is split)
             {"atom_type": "Claim", "data": {"id": "k6", "claim_kind": "commitment", "subject": "e1", "anchor": "We accept",
                                             "evidence": [{"chunk_id": "s2"}]}},
             # no subject at all, on a labelled file, right stage, for a gold deal no atom matches
             {"atom_type": "Claim", "data": {"id": "k7", "claim_kind": "stage_update", "subject": None, "anchor": "unrelated",
                                             "evidence": [{"chunk_id": "s2"}], "attributes": {"stage": "lead"}}},
             {"atom_type": "Claim", "data": {"id": "k8", "claim_kind": "commitment", "subject": None, "anchor": "We accept",
                                             "evidence": [{"chunk_id": "s2"}]}},
             {"atom_type": "Claim", "data": {"id": "k4", "claim_kind": "stage_update", "subject": "e5", "anchor": "unrelated",
                                             "evidence": [{"chunk_id": "s2"}], "attributes": {"stage": "lead"}}},
             # anchored in f/3; only without anchor narrowing would it reach f/2's "won"
             {"atom_type": "Claim", "data": {"id": "k9", "claim_kind": "stage_update", "subject": "e5", "anchor": "unrelated",
                                             "evidence": [{"chunk_id": "s2"}], "attributes": {"stage": "won"}}}]
    # e5 serves f-d1's current stage (won, f/2) from f/2; f-d2 (current lead) has no matched atom
    DOC_FILES.update({"doc-1": "f/1", "doc-2": "f/2", "doc-3": "f/3"})
    decisions = [{"type": "deal", "attribute": "stage", "atom": "e5", "outcome": "decided", "values": ["won"],
                  "documents": ["doc-2"]}]
    ent, claims = load_atlas(atoms, sec, decisions)
    got = score(g, ent, claims, g["files"])
    want = {"people": 1, "emails_split": 1, "companies": 1, "deals": 1, "stage": 1, "stage_served": 1,
            "commitments": 1, "made_up_stage": 0}
    have = {"people": got["people"]["hit"], "emails_split": got["emails_split"], "companies": got["companies"]["hit"],
            "deals": got["deals"]["hit"], "stage": got["stage"]["hit"], "stage_served": got["stage_served"]["hit"],
            "commitments": got["commitments"]["hit"], "made_up_stage": got["made_up"]["stage_updates"]}
    bad = {k: (have[k], v) for k, v in want.items() if have[k] != v}
    print(json.dumps({"instrument_version": INSTRUMENT_VERSION,
                      "selftest": "pass" if not bad else "FAIL", "mismatch": bad}))
    return 0 if not bad else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward")
    ap.add_argument("--gold", type=pathlib.Path, default=WARD / "gold")
    ap.add_argument("--json", type=pathlib.Path)
    ap.add_argument("--atoms", type=pathlib.Path, help="score this atoms.json (e.g. compose.py's output) instead of the corpus atlas")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--bar", help="print one crm-proof bar's holdout value as {value, artifact} (co-lineage measure)")
    ap.add_argument("--run", type=pathlib.Path, default=WARD / "runs/current",
                    help="the run's Phase-1 token snapshots (tokens-*.json: calls, started/updated ms) and score.json")
    a = ap.parse_args()
    if a.selftest:
        return selftest()
    secfiles = section_files(a.corpus)
    atoms_path = a.atoms or HOME / ".svrnmesh/indexes" / a.corpus / "atlas/atoms.json"
    atoms = json.loads(atoms_path.read_text())["atoms"]
    decisions = load_decisions(atoms_path.parent / "derived_decisions.jsonl")
    ent, claims = load_atlas(atoms, secfiles, decisions)
    g = load_gold(a.gold)
    done = json.loads((HOME / ".svrnmesh/enrichment" / a.corpus / "cache/questions.json").read_text())
    extracted = {q.get("chapter_id") for q in (done.get("questions_by_chapter") or done.get("chapters") or [])}
    covered = {f for s in extracted for f, _ in secfiles.get(s, [])} & g["files"]
    smoke = set((HERE / "smoke_sections.txt").read_text().strip().split(","))
    smoke_files = {f for s in smoke for f, _ in secfiles.get(s, [])}
    snaps = [json.loads(p.read_text()) for p in sorted(a.run.glob("tokens-*.json"))]
    messages = sum(len(secfiles.get(s, [])) for s in extracted)
    wall = sum(t["updated_at_ms"] - t["started_at_ms"] for t in snaps) / 1000
    scopes = scoring_scopes(g["files"], smoke_files)
    out = {"instrument_version": INSTRUMENT_VERSION,
           "deal_matching_basis": "maximum-cardinality evidence+counterparty coverage; not descriptor-level transaction identity proof",
           "gold_files": len(g["files"]), "covered_files": len(covered),
           "stage_decisions": sum(1 for e in ent.values() if "_stage" in e),
           "cost": {"phase1_wall_s": round(wall, 1), "calls": sum(t["calls"] for t in snaps),
                    "messages_extracted": messages, "s_per_message": round(wall / messages, 2) if messages and snaps else None,
                    "prompt_tokens": sum(t["prompt_tokens"] for t in snaps),
                    "completion_tokens": sum(t["completion_tokens"] for t in snaps)},
           "holdout": score(g, ent, claims, scopes["holdout"]),
           "all": score(g, ent, claims, scopes["all"]),
           "dev": score(g, ent, claims, scopes["dev"])}
    if a.bar:
        a.run.mkdir(parents=True, exist_ok=True)
        art = a.run / "score.json"
        art.write_text(json.dumps(out, indent=1) + "\n")
        key = {"crm-people": "people", "crm-companies": "companies", "crm-deals": "deals", "crm-stage": "stage_served",
               "crm-commitments": "commitments", "crm-contacts": "contacts"}.get(a.bar)
        value = out["cost"]["s_per_message"] if a.bar == "crm-cost" else (out["holdout"][key]["recall"] if key else None)
        if value is None:
            print(f"no value for {a.bar}", file=sys.stderr)
            return 4
        print(json.dumps({"value": value, "artifact": str(art), "instrument_version": INSTRUMENT_VERSION}))
        return 0
    print(json.dumps(out, indent=1))
    if a.json:
        a.json.write_text(json.dumps(out, indent=1) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
