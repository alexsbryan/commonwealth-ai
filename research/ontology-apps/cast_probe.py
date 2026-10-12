"""Cast probe (cast-probe/PREREG.md): cross-document recall per declared type RESOLVE decides, on the six runs/about
legs. R1 inventory (the ladders'), R2 proposal recall and R3 decision given proposal (from resolve_decisions.jsonl,
in RESOLVE's own document order and record handles), and R2' replayed proposers (pointed names, embeddings, declared
references), with no chat call.

    cast_probe.py [leg ...]      # scores and writes cast-probe/score.json and rows.jsonl
"""
import collections
import json
import pathlib
import re
import sys

import numpy as np

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "runs/whole-text"))
import cite_probe as C  # noqa: E402  the production embedder, cached by text
import ladder as LD  # noqa: E402
import ladder_gvc as G  # noqa: E402
import ladder_uv as LU  # noqa: E402
import ladder_ward as LW  # noqa: E402
import locate_probe as LP  # noqa: E402  find(): a gold line's reader line, as the Locate probe placed it
import score as W  # noqa: E402  runs/whole-text legs: layout() and LEGS

RUN = ROOT / "runs/about"
DATA = HERE / "cast-probe"
W.H = RUN
CORPUS = {"ward": "crm-ward", "uv": "uv-support", "gvc": "cdcr-gvc"}
GOLD_TYPE = {"ward": {"deal"}, "uv": {"case", "problem"}, "gvc": {"happening", "incident"}}  # gold's particular, per author
ABOUT = re.compile(r'document_read/about: particular document="([^"]+)" of=(\S+) line=(\d+) words=(.*)$')
MENTION = re.compile(r'document_read/mention: named document="([^"]+)" line=(\d+) of=(\S+) p=\S+ dist=.*? words=(.*)$')
HANDLE = re.compile(r'^(.*)@\d+\.\.\d+#"l(\d+)(?:-(\d+))?')
TOKEN = re.compile(r"[a-z0-9]+")
KS = (1, 3, 10)


def documents(leg, system):
    """{source doc id: (metadata, reader lines)}: chunks in order, non-blank lines, as the reader numbers them."""
    import lance  # noqa: PLC0415
    idx = RUN / leg / "data/indexes"
    corp = CORPUS[system] if (idx / CORPUS[system]).exists() else next(p.name for p in idx.iterdir() if (p / "chunks.lance").exists())
    ds = lance.dataset(str(idx / corp / "chunks.lance"))
    by = collections.defaultdict(list)
    for r in ds.to_table(columns=["id", "source_doc_id", "metadata", "content"]).to_pylist():
        by[r["source_doc_id"]].append(r)
    out = {}
    for d, ch in by.items():
        ch.sort(key=lambda r: int(r["id"]))
        out[d] = (json.loads(ch[0]["metadata"] or "{}"),
                  [ln.rstrip() for ln in "\n".join(c["content"] for c in ch).split("\n") if ln.strip()])
    return out


def pointed(leg):
    """{doc: [words]} About's and Mention's pointed words, from the leg's trace."""
    out = collections.defaultdict(list)
    for raw in open(RUN / leg / "extract.err.log", errors="replace"):
        raw = LP.ANSI.sub("", raw)
        if m := ABOUT.search(raw):
            out[m[1]].append(m[4].strip().rstrip(";"))
        elif m := MENTION.search(raw):
            out[m[1]].append(m[4].strip().rstrip(";"))
    return out


def particulars(system, docs):
    """statement handle -> set of gold particulars, by the ladders' units: Ward a message in one gold deal, uv a
    tune gold document's case, GVC the chains with a gold mention on the statement's lines."""
    if system == "uv":
        gold_doc, _, _, _ = LU.U.load_gold("tune")
        by_id = {str(m.get("id")): d for d, (m, _) in docs.items()}
        doc_case = {by_id[str(k)]: c for k, c in gold_doc.items() if str(k) in by_id}
        return lambda doc, lines: {doc_case[doc]} if doc in doc_case else set()
    if system == "ward":
        g = LW.S.load_gold(LW.S.WARD / "gold")
        of = collections.defaultdict(set)
        for dl in g["deals"]:
            for f in dl["files"]:
                of[f].add(dl["id"])
        doc_deal = {}
        for e in json.loads((LW.S.WARD / "manifest.json").read_text()):
            ds = of.get(e["path"], set())
            if len(ds) == 1:
                for mid in e.get("message_ids") or []:
                    doc_deal[LW.doc_id(mid)] = next(iter(ds))
        return lambda doc, lines: {doc_deal[doc]} if doc in doc_deal else set()
    mentions, bodies, keys = G.load_gold(G.GOLD, G.CORPUS)
    inv = {v: k for k, v in keys.items()}
    on = collections.defaultdict(set)  # (doc, reader line) -> chains
    for m in mentions.values():
        d = inv.get(m["doc"], m["doc"])
        if d not in docs:
            continue
        b = bodies[m["doc"]]
        ls, le = b.rfind("\n", 0, m["start"]) + 1, b.find("\n", m["end"])
        lines = {n: {"text": t} for n, t in enumerate(docs[d][1], 1)}
        n = LP.find(lines, b[ls:le if le >= 0 else len(b)])
        if n is not None:
            on[(d, n)].add(str(m["chain"]))
    return lambda doc, lines: set().union(*[on.get((doc, n), set()) for n in lines]) if lines else set()


def handle(h):
    m = HANDLE.match(h)
    if not m:
        return h.split("@")[0], set()
    a, b = int(m[2]), int(m[3] or m[2])
    return m[1], set(range(a, b + 1))


def decided(o):
    oc = o["outcome"]
    if "decided" in oc:
        return oc["decided"].get("record")
    return None


def names(words):
    return {" ".join(TOKEN.findall(w.lower())) for w in words if TOKEN.findall(w.lower())}


def name_score(q, r):
    """Normalised exact match 1; otherwise the best shared-token share (Jaccard) between any two names."""
    best = 0.0
    for a in q:
        for b in r:
            if a == b:
                return 1.0
            ta, tb = set(a.split()), set(b.split())
            best = max(best, len(ta & tb) / len(ta | tb))
    return best


def score_leg(leg, emb):
    system = W.LEGS[leg][0]
    docs = documents(leg, system)
    words = pointed(leg)
    part = particulars(system, docs)
    rows = [json.loads(l) for l in open(RUN / leg / "recorded/resolve_decisions.jsonl")]
    meta_by_doc = {d: m for d, (m, _) in docs.items()}
    # declared references: uv target_url names an issue (its documents share `thread`); Ward base_subject
    thread_docs = collections.defaultdict(set)
    for d, m in meta_by_doc.items():
        if system == "uv":
            thread_docs[str(m.get("thread"))].add(d)
        elif system == "ward" and m.get("base_subject") not in (None, "", "None"):
            thread_docs[m["base_subject"]].add(d)

    def refs(d):
        m = meta_by_doc.get(d, {})
        if system == "uv":
            t = re.search(r"/(?:issues|pull)/(\d+)", str(m.get("target_url") or ""))
            return thread_docs.get(t[1], set()) if t else set()
        if system == "ward":
            return thread_docs.get(m.get("base_subject"), set()) - {d}
        return None  # GVC declares no reference: never-ran

    def text_of(d, lines):
        raw = docs.get(d, ({}, []))[1]
        return " ".join(raw[n - 1] for n in sorted(lines) if 0 < n <= len(raw))

    rec_of, rec_docs, rec_strings = {}, collections.defaultdict(set), collections.defaultdict(set)
    earlier = collections.defaultdict(list)  # particular -> statements of earlier documents
    agg = collections.defaultdict(lambda: collections.Counter())
    out_rows = []
    gold_types = GOLD_TYPE[system]
    for row in rows:
        if row.get("type") not in gold_types:
            continue
        d, cands = row["document"], set(row.get("candidates") or [])
        open_records = set(rec_docs)
        unit_strings = set(words.get(d, []))
        pend = []
        for o in row["outcomes"]:
            s = o["statement"]
            sdoc, lines = handle(s)
            ps = part(sdoc, lines)
            rec = decided(o)
            q_strings = unit_strings | ({text_of(sdoc, lines)} - {""})
            ranked = {}
            if open_records and ps:
                qn = names(q_strings)
                ns = {r: name_score(qn, names(rec_strings[r])) for r in open_records}
                qv = [emb.unit(t) for t in q_strings if t]
                es = {r: max((float(a @ emb.unit(b)) for a in qv for b in rec_strings[r] if b), default=0.0) for r in open_records}
                rf = refs(sdoc)
                ds = None if rf is None else {r: float(bool(rec_docs[r] & rf)) for r in open_records}
                ranked = {"N": ns, "E": es} | ({"D": ds} if ds is not None else {})
            argmax = None
            ch = o.get("choice") or {}
            if ch.get("candidates"):
                best = max(ch["candidates"], key=lambda x: x[1])
                argmax = best[0] if best[1] > ch.get("none", 0) else None
            for p in ps:
                right = {rec_of[t] for t in earlier[p] if rec_of.get(t)}
                a = agg[system]
                if not right:
                    a["new"] += 1
                    a["new_opened"] += rec == s
                    continue
                a["given"] += 1
                offered = bool(right & cands)
                a["R2"] += offered
                if offered:
                    a["offered"] += 1
                    a["R3"] += rec in right
                    a["argmax_right"] += argmax in right
                    a["override"] += (argmax in right) and rec not in right
                r_out = {"leg": leg, "doc": d, "statement": s, "particular": p, "offered": offered, "linked": rec in right,
                         "candidates": len(cands)}
                for name, sc in ranked.items():
                    order = sorted(sc, key=lambda r: -sc[r])
                    for k in KS:
                        top = set(order[:k]) if any(sc.values()) else set()
                        hit = bool(right & top)
                        a[f"{name}@{k}"] += hit
                        a[f"{name}+logged@{k}"] += bool(right & (top | cands))
                        a[f"{name}+logged@{k}_shown"] += len(top | cands)
                    r_out[f"{name}_rank"] = next((i + 1 for i, r in enumerate(order) if r in right), None)
                if "D" not in ranked:
                    a["D_never_ran"] += 1
                out_rows.append(r_out)
            pend.append((s, rec, ps, q_strings))
        for s, rec, ps, q_strings in pend:  # this document's statements join the cast after it is decided
            rec_of[s] = rec
            if rec:
                rec_docs[rec].add(handle(s)[0])
                rec_strings[rec] |= q_strings
            for p in ps:
                earlier[p].append(s)
    return agg[system], out_rows


# ---------------------------------------------------------------- Amendment 1: Ward company mentions (the party slice)
ADDRESS = re.compile(r"[\w.+'-]+@([\w-]+(?:\.[\w-]+)+)")


def company_section(leg, emb):
    """Per Ward message in one gold deal: is the counterparty C offered by the message's header records (F), by a
    Mention pick (M), and by name or embedding against the company records open before it (N, E)?"""
    import email.utils  # noqa: PLC0415
    S = LW.S
    g = S.load_gold(S.WARD / "gold")
    comp = {c["id"]: c for c in g["companies"]}
    forms = {cid: S.gold_forms(c) for cid, c in comp.items()}
    ours = {cid for cid, c in comp.items() if "enron.com" in (c.get("domains") or [])}

    def resolves(text):
        """Gold companies a written name or an address/domain resolves to (the scorer's rule: a shared domain, else
        an equal name core)."""
        dom = {m.lower() for m in ADDRESS.findall(text)} | ({text.lower()} if re.fullmatch(r"[\w-]+(\.[\w-]+)+", text.strip()) else set())
        hit = {cid for cid, c in comp.items() if dom & {x.lower() for x in c.get("domains") or []}}
        core = S.name_core(text)
        return hit | {cid for cid, f in forms.items() if core and core in f}

    docs = documents(leg, "ward")
    of = collections.defaultdict(set)
    for dl in g["deals"]:
        for f in dl["files"]:
            of[f].add(dl["id"])
    deal_of = {}
    for e in json.loads((S.WARD / "manifest.json").read_text()):
        ds = of.get(e["path"], set())
        if len(ds) == 1:
            for mid in e.get("message_ids") or []:
                deal_of[LW.doc_id(mid)] = next(iter(ds))
    cp = {dl["id"]: dl.get("counterparty") for dl in g["deals"]}
    mentions = collections.defaultdict(list)
    for raw in open(RUN / leg / "extract.err.log", errors="replace"):
        raw = LP.ANSI.sub("", raw)
        if (m := MENTION.search(raw)) and m[3] == "company":
            mentions[m[1]].append(m[4].strip().rstrip(";"))

    def when(d):
        try:
            return email.utils.parsedate_to_datetime(docs[d][0].get("date") or "").timestamp()
        except (TypeError, ValueError):
            return float("inf")

    def header_domains(d):
        m = docs[d][0]
        return {x.lower() for f in ("from", "to", "cc") for x in ADDRESS.findall(str(m.get(f) or ""))}

    read = set()
    for raw in open(RUN / leg / "extract.err.log", errors="replace"):
        if (m := re.search(r'document_read/[a-z_]+: [^=]*document="?([^" ]+)"?', LP.ANSI.sub("", raw))):
            read.add(m[1])

    def partly(w, cid):
        """Diagnostic added after the strict count was read: the mention's words share a token with C's written forms
        or a domain stem (the link step's reach, not the scorer's rule)."""
        toks = set(S.name_core(w).split())
        stems = {x.split(".")[0].lower() for x in comp[cid].get("domains") or []}
        return bool(toks & (set(" ".join(forms[cid]).split()) | stems))

    a = collections.Counter()
    open_records = {}  # record name -> gold companies it resolves to (header domains and earlier mention names)
    for d in sorted(docs, key=when):
        dom = header_domains(d)
        ms = mentions.get(d, [])
        c = cp.get(deal_of.get(d))
        if c in comp and d in read:
            a["units"] += 1
            a["M_partly"] += any(partly(w, c) for w in ms)
            f_hit = any(c in resolves(x) for x in dom)
            m_hit = [w for w in ms if c in resolves(w)]
            a["F"] += f_hit
            a["M"] += bool(m_hit)
            a["F_or_M"] += f_hit or bool(m_hit)
            given = [r for r, cs in open_records.items() if c in cs]
            if m_hit and given:
                a["link_units"] += 1
                q = m_hit[0]
                names_ = list(open_records)
                n_sc = {r: name_score(names([q]), names([r])) for r in names_}
                emb.need([q] + names_)
                qv = emb.unit(q)
                e_sc = {r: float(qv @ emb.unit(r)) for r in names_}
                for nm, sc in (("N", n_sc), ("E", e_sc)):
                    order = sorted(names_, key=lambda r: -sc[r])
                    for k in KS:
                        a[f"{nm}@{k}"] += any(c in open_records[r] for r in order[:k])
                a["link_records_open"] += len(names_)
        for w in ms:
            hit = resolves(w)
            if hit & ours:
                a["ours_mentions"] += 1
                a["ours_mentions_F_offers"] += any(resolves(x) & hit & ours for x in dom)
        for x in dom:
            open_records.setdefault(x, resolves(x))
        for w in ms:
            open_records.setdefault(w, resolves(w))
    u, lu = a["units"] or 1, a["link_units"] or 1
    res = {"units": a["units"], "F": [a["F"], a["units"]], "M": [a["M"], a["units"]],
           "M_partly_diagnostic": [a["M_partly"], a["units"]], "F_or_M": [a["F_or_M"], a["units"]],
           "link_units": a["link_units"], "mean_records_open": round(a["link_records_open"] / lu, 1),
           "ours_mentions": a["ours_mentions"], "ours_mentions_F_offers": a["ours_mentions_F_offers"]}
    for nm in ("N", "E"):
        for k in KS:
            res[f"{nm}@{k}"] = [a[f"{nm}@{k}"], a["link_units"]]
    return res


def main():
    emb = C.Embedder()
    if sys.argv[1:2] == ["--companies"]:
        out = {leg: company_section(leg, emb) for leg in ("ours-ward", "blind-ward")}
        print(json.dumps(out, indent=1))
        (DATA / "companies.json").write_text(json.dumps(out, indent=1) + "\n")
        return
    legs = sys.argv[1:] or list(W.LEGS)
    report, all_rows = {}, []
    for leg in legs:
        system = W.LEGS[leg][0]
        # embed every string the replay will compare, once, before scoring
        docs = documents(leg, system)
        words = pointed(leg)
        texts = {w for ws in words.values() for w in ws}
        for row in (json.loads(l) for l in open(RUN / leg / "recorded/resolve_decisions.jsonl")):
            for o in row["outcomes"]:
                sdoc, lines = handle(o["statement"])
                raw = docs.get(sdoc, ({}, []))[1]
                t = " ".join(raw[n - 1] for n in sorted(lines) if 0 < n <= len(raw))
                if t:
                    texts.add(t)
        emb.need(sorted(texts))
        a, rows = score_leg(leg, emb)
        g = a["given"] or 1
        res = {"given": a["given"], "new": a["new"], "novelty_opened": [a["new_opened"], a["new"]],
               "R2": round(a["R2"] / g, 3), "R3_given_offered": [a["R3"], a["offered"]],
               "model_argmax_right_given_offered": [a["argmax_right"], a["offered"]],
               "weigher_override": a["override"]}
        for name in ("N", "E", "D"):
            if name == "D" and a["D_never_ran"]:
                res["D"] = "never-ran (no declared reference)"
                continue
            for k in KS:
                res[f"{name}@{k}"] = round(a[f"{name}@{k}"] / g, 3)
                res[f"{name}+logged@{k}"] = round(a[f"{name}+logged@{k}"] / g, 3)
                res[f"{name}+logged@{k}_mean_shown"] = round(a[f"{name}+logged@{k}_shown"] / g, 2)
        ladder = LD.run_system(system, W.layout(leg))
        res["R1_identity"] = {k: ladder["identity"].get(k) for k in ("coverage", "b_cubed", "recovery_b_cubed")}
        report[leg] = res
        all_rows += rows
        print(json.dumps({leg: res}), flush=True)
    DATA.mkdir(exist_ok=True)
    (DATA / "score.json").write_text(json.dumps(report, indent=1) + "\n")
    (DATA / "rows.jsonl").write_text("".join(json.dumps(r) + "\n" for r in all_rows))


if __name__ == "__main__":
    main()
