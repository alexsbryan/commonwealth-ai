"""misses.py <run dir> <corpus>: each unserved gold deal, traced from gold to record, with a verdict.

Reads only what loop.sh wrote: extract.err (every window's prompt and raw answer, at trace), resolve.err (RESOLVE's
decisions, at debug) and score.json (score.py's report). Splits the windows into <run dir>/windows/ for reading.
Writes <run dir>/misses.jsonl and prints the same as text.

A gold update's chain: the window and label its message was shown under; VISIBLE when most of the gold quote's
words are in the lines the window showed (score.py's matching rule); the window claims whose quote matches it,
each with its particular's gold identity (the deal most of that particular's stage statements are); the records
holding a statement score.py matched to it. A deal's verdict is the earliest step that breaks serving, in pipeline
order: what the reader saw, what it wrote, its particulars, validation, RESOLVE, the fold. Mechanisms:
  deciding_update_hidden  the update that decides the stage was not in the lines the window showed
  two_deal_message        the message's statement went to another deal of the same message, none to this one, or
                          one claim quotes this deal's statement and another deal's together
  misattributed           this deal's statement went to a particular that is another deal (in-window identity)
  wrong_kind              the statement was written, as another kind than stage_update
  other_sentence_cited    a stage statement about this deal cites another sentence of the message (or the scorer misses)
  not_emitted             shown, and nothing about it written
  stage_misread           written about this deal with another stage
  refused                 written right, refused by validation
  in_window_merge         one particular carries statements of this deal and another
  in_window_split         one window gave this deal two particulars
  cross_window_merge      RESOLVE joined this deal's particular to another deal's record
  cross_window_split      RESOLVE left this deal's particulars in several records
  later_claim_in_record   the deciding statement is on the record, and a later-dated one there sets the stage
A verdict is the first break, never the only one: `also` lists the deal's other breaks. Reader steps come
before RESOLVE's: the deciding update's emission, then the reader's particulars, then RESOLVE's joins, then the
fold; a record whose stage is set by another deal's statement is that merge, not a fold fault.
Each update also carries its REFERENT: where the words that tell its deal from the folder's other gold deals
(its gold description's, less theirs) stand: own_lines (the lines the window showed), subject_only (only the
base_subject), quoted_only (only the hidden quoted or embedded text of the raw message), nowhere.

Every verdict has a ROOT, the cause a fix is aimed at (a closed set, `ROOT`; table.py rolls them up by whether the
one-shot serves the deal):
  hidden            the deciding update was not in the lines the reader was shown
  hidden_referent   the reader put the statement on another deal, and what tells the deals apart is only in the
                    hidden quoted text
  end_unwritten     the deal's ending (lost) statement was not written: a replacement or rollover read as identity
  omitted           a statement the reader was shown and did not write
  short_span        the right statement, cited by a span too short for the scorer (a part of the gold sentence);
                    the detail says whether its record is right in substance
  identity          the reader's particulars merge or split deals within a window
  stage             written with another stage, or as another kind
  resolve           RESOLVE's join across windows; the detail carries decisions.py's verdict on the question
  fold              right record, a later statement sets its stage
  scorer            the record is right and score.py's 60% rule credits a sibling deal's sentence to it, or the
                    record ties on a sentence gold files under two deals and nothing of the deal's own went astray
  other_sentence / validation / unclassified
Each update carries its evidence: `thought`, the reader's reasoning lines that name the update's document label;
`oneshot`, what the one-shot wrote for the same update; `judged`, decisions.py's verdict on its RESOLVE question.
"""
import json, os, re, sys
from collections import Counter
from email.utils import parsedate_to_datetime
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpora

STAGES = ("lead", "proposal", "negotiating", "won", "lost")
STOP = set("a an the and or of to in on for at by with from as is are was were be been it its this that these those we you i he she they them our your my me us if so but not no do does did have has had will would can could should please any all also there here than then just about into over under up out".split())
ROOT = dict(sibling_credit="scorer", shared_sentence_tie="scorer", shared_sentence_misrouted="resolve", deciding_update_hidden="hidden", two_deal_message="omitted", not_emitted="omitted", short_span="short_span",
            other_sentence_cited="other_sentence", misattributed="identity", in_window_merge="identity", in_window_split="identity",
            stage_misread="stage", wrong_kind="stage", refused="validation", cross_window_merge="resolve", cross_window_split="resolve",
            later_claim_in_record="fold")
KIND = dict(deciding_update_hidden="MISSING_EVIDENCE", short_span="MISREADING", two_deal_message="MISREADING", misattributed="MISREADING",
            wrong_kind="MISREADING", other_sentence_cited="MISREADING", not_emitted="MISREADING", stage_misread="MISREADING",
            in_window_merge="MISREADING", in_window_split="MISREADING", refused="HANDOFF", cross_window_merge="HANDOFF",
            cross_window_split="HANDOFF", later_claim_in_record="HANDOFF")
HEADER = re.compile(r"^(date|base_subject|message_id|from|to|cc|bcc|subject|Named here):")
LOGLINE = re.compile(r"^\d{4}-\d\d-\d\dT[\d:.]+Z\s+(TRACE|DEBUG|INFO|WARN|ERROR)\s")


def words(s): return {w for w in re.findall(r"[a-z0-9]+", (s or "").lower()) if w not in STOP and len(w) > 1}
def norm(s): return re.sub(r"\s+", " ", s or "").strip()
def slug(mid): return re.sub(r"[^a-z0-9]+", "-", mid.lower()).strip("-")
def pdate(s): return parsedate_to_datetime(re.sub(r"\s*\(.*\)$", "", s.strip()))
def covers(gold_quote, text):
    g = words(gold_quote)
    return len(g) >= 2 and len(g & words(text)) / len(g) >= 0.6


def part_of(gold_quote, text):
    """The quote is a span of the gold sentence: most of its words are the sentence's, too few to cover it."""
    g, c = words(gold_quote), words(text)
    n = len(g & c)
    return n >= 3 and n / max(1, len(c)) >= 0.6 and not covers(gold_quote, text)


def windows(run):
    """{chapter: {labels, prompt, answer, reasoning}} from extract.err's traces; written out to <run>/windows/ once.
    A reasoning engine's answer opens with a <think> block, as the reader's own parser sees it (prepare_phase_json):
    the JSON is what follows the last </think>; the reasoning goes to <chapter>.reasoning.txt."""
    W, cur, field, raw = {}, None, None, {}
    for line in open(f"{run}/extract.err", errors="replace"):
        if cur is not None:
            if LOGLINE.match(line): cur = None
            else:
                if field == "prompt": W[cur]["prompt"] += line
                else: raw[cur] += line
                continue
        m = re.search(r"TRACE document_read/window: prompt chapter=(\S+) labels=(.*?) prompt=(.*)$", line, re.S)
        if m:
            cur, field = m.group(1), "prompt"
            W[cur] = dict(labels=dict(x.split("=", 1) for x in m.group(2).split()), prompt=m.group(3), answer=None, reasoning="")
            continue
        m = re.search(r"TRACE document_read/window: answer chapter=(\S+) answer=(.*)$", line, re.S)
        if m: cur, field = m.group(1), "answer"; raw[cur] = m.group(2)
    for ch, text in raw.items():
        w = W.setdefault(ch, dict(labels={}, prompt="", reasoning=""))
        if "</think>" in text:
            head, text = text.rsplit("</think>", 1)
            w["reasoning"] = head.replace("<think>", "", 1).strip()
        try: w["answer"] = json.loads(text.strip())
        except ValueError: w["answer"] = {"unparsed": text[:2000]}
    os.makedirs(f"{run}/windows", exist_ok=True)
    for ch, w in W.items():
        open(f"{run}/windows/{ch}.labels.txt", "w").write("".join(f"{k}={v}\n" for k, v in w["labels"].items()))
        open(f"{run}/windows/{ch}.prompt.txt", "w").write(w["prompt"])
        json.dump(w["answer"], open(f"{run}/windows/{ch}.answer.json", "w"), indent=1)
        if w.get("reasoning"): open(f"{run}/windows/{ch}.reasoning.txt", "w").write(w["reasoning"])
    return W


def refusals(run):
    """{(chapter, document, kind)}: the window reader's and validation's refused claims, by document and kind."""
    out, ch = Counter(), None
    for line in open(f"{run}/extract.err", errors="replace"):
        m = re.search(r"document_read/window: answer chapter=(\S+)", line)
        if m: ch = m.group(1); continue
        m = re.search(r"(?:claim refused|document_read_claim_refused) document=(\S+) kind=(\S+)", line)
        if m: out[(ch, m.group(1), m.group(2))] += 1
    return out


def resolve_lines(run):
    """{(document, chapter/ref): [RESOLVE debug lines]}."""
    out = {}
    if not os.path.exists(f"{run}/resolve.err"): return out
    for line in open(f"{run}/resolve.err", errors="replace"):
        m = re.search(r'document="([^"]+)" statement=\S+?#"([^"]+)"', line)
        if m: out.setdefault((m.group(1), m.group(2)), []).append(re.sub(r"^\S+\s+DEBUG\s+", "", line.strip())[:400])
    return out


def load(run, cid):
    c = corpora.corpus(cid)
    doc_of_file = {k: slug(v) for k, v in json.load(open(c["msgids"])).items() if v}
    deals, gold = {}, []
    for f in c["folders"]:
        g = json.load(open(f"{c['gold']}/{f}.json"))
        for d in g["deals"]: deals[d["id"]] = dict(d, folder=f)
        for s in g["stage_updates"]:  # score.py's order, so its `matches` indices are these
            gold.append(dict(s, folder=f, doc=doc_of_file.get(f"{f}/{s['file']}"), dt=pdate(s["date"])))
    for i, u in enumerate(gold): u["i"] = i
    W, refused = windows(run), refusals(run)
    shown, where, claims, subject, body = {}, {}, [], {}, {}
    for ch in sorted(W):
        w = W[ch]
        for m in re.finditer(r"^=== (d\d+) ===\n(.*?)(?=^=== d\d+ ===\n|\Z)", w["prompt"], re.M | re.S):
            doc = w["labels"].get(m.group(1)); shown[doc] = norm(m.group(2)); where[doc] = (ch, m.group(1))
            lines = m.group(2).split("\n")
            subject[doc] = next((l.split(":", 1)[1] for l in lines if l.startswith("base_subject:")), "")
            body[doc] = "\n".join(l for l in lines if not HEADER.match(l))
        ans = w["answer"] or {}
        parts = {p.get("ref"): p for p in ans.get("particulars", []) if isinstance(p, dict)}
        dates = {m.group(1): pdate(m.group(2)) for m in re.finditer(r"^=== (d\d+) ===\ndate: (.*)$", w["prompt"], re.M)}
        seen = Counter()
        for k, x in enumerate(ans.get("claims", [])):
            if not isinstance(x, dict): continue
            doc = w["labels"].get(x.get("document")); kind = x.get("kind"); seen[(doc, kind)] += 1
            p = parts.get(x.get("about")) or {}
            claims.append(dict(ch=ch, n=k, kind=kind, about=x.get("about"), about_type=p.get("type"), about_name=p.get("name"),
                               doc=doc, label=x.get("document"), dt=dates.get(x.get("document")), quote=x.get("quote") or "",
                               stage=(x.get("fields") or {}).get("stage"),
                               refused=seen[(doc, kind)] <= refused[(ch, doc, kind)],
                               hits=[u["i"] for u in gold if doc and u["doc"] == doc and covers(u["quote"], x.get("quote"))],
                               parts=[u["i"] for u in gold if doc and u["doc"] == doc and part_of(u["quote"], x.get("quote"))]))
    # A particular's gold identity: the deal most of its stage statements are (a statement citing a span of a gold
    # sentence counts for it). A tie names no deal: the particular is two deals as much as one.
    units = {}
    for cl in claims:
        if cl["kind"] == "stage_update" and not cl["refused"]: units.setdefault((cl["ch"], cl["about"]), []).append(cl)
    of = lambda cl: set(cl["hits"]) | set(cl["parts"])
    ident, unit_deals = {}, {key: {gold[h]["deal"] for cl in cls for h in of(cl)} for key, cls in units.items()}
    for key, cls in units.items():
        votes = Counter(gold[h]["deal"] for cl in cls for h in of(cl))
        if votes:
            top = max(votes.values()); tied = sorted(g for g, n in votes.items() if n == top)
            if len(tied) == 1: ident[key] = tied[0]
    S = json.load(open(f"{run}/score.json"))
    # Each placed statement back onto its window claim: same document, the claim's quote inside its match text.
    # One quote written about two particulars (one sentence, two deals) is told apart by the record the statement
    # landed on: the claim whose particular that record's other statements come from.
    unit_of_cid, by_doc, used, pending = {}, {}, Counter(), {}
    for cl in claims:
        if cl["kind"] == "stage_update" and not cl["refused"]: by_doc.setdefault(cl["doc"], []).append(cl)
    for cid, doc in S["claim_doc"].items():
        text = norm(S["claim_match_text"].get(cid)).lower()
        cands = [cl for cl in by_doc.get(doc, []) if norm(cl["quote"]) and norm(cl["quote"]).lower() in text]
        if cands:
            best = max(len(cl["quote"]) for cl in cands)
            top = [cl for cl in cands if len(cl["quote"]) == best]
            if len({(cl["ch"], cl["about"]) for cl in top}) == 1: unit_of_cid[cid] = (top[0]["ch"], top[0]["about"])
            else: pending[cid] = top
    rec_of = {cid: r for r, cids in S["placed"].items() for cid in cids}
    for cid, top in pending.items():
        mates = {unit_of_cid[c] for c in S["placed"].get(rec_of.get(cid), []) if c in unit_of_cid}
        cl = sorted(top, key=lambda cl: ((cl["ch"], cl["about"]) not in mates, used[id(cl)]))[0]
        used[id(cl)] += 1; unit_of_cid[cid] = (cl["ch"], cl["about"])
    oneshot = json.load(open(c["oneshot_records"])).get("records", []) if os.path.exists(c.get("oneshot_records", "")) else []
    return dict(c=c, deals=deals, gold=gold, shown=shown, where=where, claims=claims, ident=ident, unit_deals=unit_deals, units=units,
                S=S, subject=subject, body=body, reasoning={ch: w.get("reasoning", "") for ch, w in W.items()}, oneshot=oneshot, judged={},
                unit_of_cid=unit_of_cid, resolve=resolve_lines(run))


def served(S, g):
    recs = {d["id"]: d for d in S["deals"]}; v = S["gold"][g]
    return len(v["records"]) == 1 and recs[v["records"][0]]["gold"] == [g] and not recs[v["records"][0]]["strays"] \
        and recs[v["records"][0]]["stage"] == v["gold_stage"]


def referent(D, u):
    """Where the words that tell u's deal from its folder's other gold deals stand, as the reader could see them."""
    g = D["deals"][u["deal"]]
    others = set().union(*[words(d.get("description")) for d in D["deals"].values() if d["folder"] == g["folder"] and d["id"] != g["id"]])
    dist = words(g.get("description")) - others
    raw_p = f"{D['c']['story']}/{u['folder']}/{u['file']}"
    raw = open(raw_p, errors="replace").read() if os.path.exists(raw_p) else ""
    own, subj, rest = dist & words(D["body"].get(u["doc"])), dist & words(D["subject"].get(u["doc"])), dist & words(raw)
    where = "own_lines" if own else "subject_only" if subj else "quoted_only" if rest else "nowhere"
    return dict(where=where, distinguishing=sorted(dist), own_lines=sorted(own), subject=sorted(subj), raw=sorted(rest))


def thought(D, ch, label, quote):
    """The reader's reasoning lines that name the update's document label, those sharing most of its words first."""
    text = D["reasoning"].get(ch) or ""
    if not text or not label: return []
    gw = words(quote)
    lines = [l.strip() for l in text.split("\n") if re.search(rf"\b{label}\b", l)]
    return [l[:300] for l in sorted(lines, key=lambda l: -len(words(l) & gw))[:3]]


def oneshot_of(D, u):
    """What the one-shot wrote for the update: each claim of its, on the same file, citing the gold sentence or a span of it."""
    key = f"{u['folder']}/{u['file']}".rstrip(".")
    return [f"{(c.get('attributes') or {}).get('stage')} on {r.get('name')!r}: {(c.get('quote') or '')[:100]!r}"
            for r in D["oneshot"] for c in r.get("claims", [])
            if (c.get("file") or "").rstrip(".") == key and (covers(u["quote"], c.get("quote")) or part_of(u["quote"], c.get("quote")))]


def records_of(D, cls):
    """The records holding these window claims' placed statements."""
    S = D["S"]; units = {(cl["ch"], cl["about"]) for cl in cls}; docs = {cl["doc"] for cl in cls}
    return sorted({r for r, cids in S["placed"].items() for cid in cids if D["unit_of_cid"].get(cid) in units and S["claim_doc"].get(cid) in docs})


def chain(D, u):
    """One gold update from the window it was shown in to the records holding it."""
    S, g = D["S"], u["deal"]
    ch, label = D["where"].get(u["doc"], (None, None))
    mine = [cl for cl in D["claims"] if u["i"] in cl["hits"]]
    on_doc = [cl for cl in D["claims"] if cl["doc"] == u["doc"] and cl["kind"] == "stage_update" and u["i"] not in cl["hits"]]
    recs = sorted({r for r, cids in S["placed"].items() for cid in cids if u["i"] in S["matches"].get(cid, [])})
    view = lambda cl: dict(kind=cl["kind"], about=f"{cl['ch']}/{cl['about']}", about_name=cl["about_name"],
                           particular_is=D["ident"].get((cl["ch"], cl["about"])), stage=cl["stage"],
                           quote=cl["quote"][:160], refused=cl["refused"])
    for_g = [cl for cl in mine if cl["kind"] == "stage_update" and not cl["refused"] and D["ident"].get((cl["ch"], cl["about"])) in (g, None)]
    for_other = [cl for cl in mine if cl["kind"] == "stage_update" and not cl["refused"] and D["ident"].get((cl["ch"], cl["about"])) not in (g, None)]
    spans = lambda cl: len({D["gold"][h]["deal"] for h in cl["hits"]}) > 1
    part = [cl for cl in on_doc if not cl["refused"] and u["i"] in cl["parts"] and D["ident"].get((cl["ch"], cl["about"])) in (g, None)]
    if ch is None or not covers(u["quote"], D["shown"].get(u["doc"], "")) and not mine: status = "hidden"
    elif [cl for cl in mine if cl["kind"] == "stage_update"] and all(spans(cl) for cl in mine if cl["kind"] == "stage_update"):
        status = "two_deal_message"  # one claim quotes this deal's statement and another deal's
    elif for_g: status = "emitted" if any(cl["stage"] == u["stage"] for cl in for_g) else "stage_misread"
    elif for_other:
        other_gold_here = {x["deal"] for x in D["gold"] if x["doc"] == u["doc"] and x["deal"] != g}
        status = "two_deal_message" if other_gold_here & {D["ident"][(cl["ch"], cl["about"])] for cl in for_other} else "misattributed"
    elif any(cl["kind"] == "stage_update" and cl["refused"] for cl in mine): status = "refused"
    elif mine: status = "wrong_kind"
    elif part: status = "short_span"
    elif any(D["ident"].get((cl["ch"], cl["about"])) in (g, None) for cl in on_doc): status = "other_sentence_cited"
    elif {D["ident"].get((cl["ch"], cl["about"])) for cl in on_doc} & {x["deal"] for x in D["gold"] if x["doc"] == u["doc"] and x["deal"] != g}:
        status = "two_deal_message"
    elif any(cl["doc"] == u["doc"] and cl["kind"] != "stage_update" for cl in D["claims"]): status = "wrong_kind"
    else: status = "not_emitted"
    return dict(file=f"{u['folder']}/{u['file']}", date=u["date"], stage=u["stage"], quote=u["quote"][:200],
                window=f"{ch} {label}" if ch else None, visible=bool(ch) and covers(u["quote"], D["shown"].get(u["doc"], "")),
                status=status, claims=[view(cl) for cl in mine], other_statements_on_message=[view(cl) for cl in on_doc],
                records=recs, referent=referent(D, u), resolve=[l for cl in mine for l in D["resolve"].get((u["doc"], f"{cl['ch']}/{cl['about']}"), [])],
                spans=[view(cl) for cl in part], span_records=records_of(D, part), thought=thought(D, ch, label, u["quote"]),
                oneshot=oneshot_of(D, u), judged=[dict(about=f"{cl['ch']}/{cl['about']}", **D["judged"][(u["doc"], f"{cl['ch']}/{cl['about']}")])
                                                for cl in mine + part if (u["doc"], f"{cl['ch']}/{cl['about']}") in D["judged"]])


def intruders(D, units, g):
    """The other deals' statements on these particulars of g's, each with its referent: why the reader merged them."""
    out = []
    for key in sorted(units):
        for cl in D["units"].get(key, []):
            for h in cl["hits"]:
                u = D["gold"][h]
                if u["deal"] != g: out.append(f"{u['deal']} {u['folder']}/{u['file']} on {key[0]}/{key[1]} (referent {referent(D, u)['where']})")
    return sorted(set(out))


def fit(D, cid, h):
    """How much of the placed statement's own words gold update h's sentence holds (score.py matches by the gold side)."""
    cw = words(D["S"]["claim_match_text"].get(cid))
    return len(words(D["gold"][h]["quote"]) & cw) / max(1, len(cw))


def credit(D, cid, deal):
    """Why score.py credits the placed statement to `deal`: `own` (a sentence of the deal's fits it best, alone or
    with another deal's, `shared`, when gold files that sentence under both), or `sibling` (another deal's sentence
    fits it better: the 60% rule credits a sibling deal's statement, a scorer artefact). None: no credit."""
    hs = D["S"]["matches"].get(cid, [])
    mine = [fit(D, cid, h) for h in hs if D["gold"][h]["deal"] == deal]
    if not mine: return None
    rest = [fit(D, cid, h) for h in hs if D["gold"][h]["deal"] != deal]
    if not rest or max(mine) > max(rest) + 1e-9: return "own"
    return "shared" if abs(max(mine) - max(rest)) < 1e-9 else "sibling"


def verdict(D, g):
    S = D["S"]; recs = {d["id"]: d for d in S["deals"]}; v = S["gold"][g]
    ups = sorted([u for u in D["gold"] if u["deal"] == g], key=lambda u: u["dt"])  # score.py's current stage: last by date
    chains = [chain(D, u) for u in ups]; last = chains[-1]
    R = v["records"]
    units_in = lambda r, deal: {D["unit_of_cid"][cid] for cid in S["placed"][r] if cid in D["unit_of_cid"]
                                and any(D["gold"][h]["deal"] == deal for h in S["matches"].get(cid, []))}
    reader, resolve, fold, scorer = [], [], [], []
    cids_on = lambda r, deal: [cid for cid in S["placed"][r] if any(D["gold"][h]["deal"] == deal for h in S["matches"].get(cid, []))]
    # A record holds g, or another deal, only by sibling credit: drop it from the reading, and say so.
    loose = {r for r in R if cids_on(r, g) and all(credit(D, cid, g) == "sibling" for cid in cids_on(r, g))}
    if loose and len(R) - len(loose) <= 1:
        scorer.append(("sibling_credit", f"{sorted(loose)} hold {g} only by a sibling deal's sentence the 60% rule credits to it"))
    for r in R:
        others = sorted(set(recs[r]["gold"]) - {g}) + [x for st in recs[r]["strays"] for x in st if x != g]
        if others and all(credit(D, cid, x) == "sibling" for x in set(others) for cid in cids_on(r, x)):
            scorer.append(("sibling_credit", f"record {r} holds {sorted(set(others))} only by sentences of {g}'s the 60% rule credits to them"))
        elif others and all(credit(D, cid, x) in ("shared", "sibling") for x in set(others) for cid in cids_on(r, x)):
            # The tie is RESOLVE's when a statement only one of the tied deals' went to the wrong record: g's own
            # elsewhere, or another deal's onto a record of g's.
            away = sorted({f"{c['file']} on {rr}" for c in chains for rr in c["records"] if rr not in R}
                          | {f"{x}'s statement on {rr}" for rr in R for x in {y for st in recs[rr]["strays"] for y in st} - {g}})
            if away: resolve.append(("shared_sentence_misrouted", f"record {r} ties {g} with {sorted(set(others))} on sentences gold files under both; "
                                     f"a statement only one of them has went astray: {away}"))
            else: scorer.append(("shared_sentence_tie", f"record {r} ties {g} with {sorted(set(others))} on sentences gold files under both"))
    if scorer or any(m == "shared_sentence_misrouted" for m, _ in resolve):
        R = [r for r in R if r not in loose]
    if len(R) > 1 and not scorer and not any(m == "shared_sentence_misrouted" for m, _ in resolve):  # else the split is the tie's
        chs = [{u[0] for u in units_in(r, g)} for r in R]
        shared = any(chs[i] & chs[j] for i in range(len(chs)) for j in range(i + 1, len(chs)))
        main = max(R, key=lambda r: len(cids_on(r, g)))
        refs = sorted({c["referent"]["where"] for c in chains if c["records"] and main not in c["records"]})
        (reader if shared else resolve).append(("in_window_split" if shared else "cross_window_split", f"{len(R)} records: {R}"
                                                + (f" (referent {'quoted_only' if 'quoted_only' in refs else refs[0]})" if shared and refs else "")))
    merged = bool(scorer) or any(m == "shared_sentence_misrouted" for m, _ in resolve)
    for r in R:
        others = sorted(set(recs[r]["gold"]) - {g}) + [x for st in recs[r]["strays"] for x in st if x != g]
        if others and not merged:
            merged = True
            mixed = units_in(r, g) & set().union(*(units_in(r, h) for h in others))
            (reader if mixed else resolve).append(("in_window_merge" if mixed else "cross_window_merge", f"record {r} also holds {sorted(set(others))}"
                + (f"; intruding: {intruders(D, mixed, g)}" if mixed else "")))
    stage_ok = bool(R) and v["gold_stage"] in v["record_stages"]
    if last["status"] != "emitted":  # the reader's own reading of the deciding update comes before its particulars
        b = ("deciding_update_hidden" if last["status"] == "hidden" else last["status"], f"deciding update {last['file']} ({last['stage']}): {last['status']}")
        if last["status"] == "short_span":
            right = [r for r in last["span_records"] if recs[r]["stage"] == v["gold_stage"] and not set(recs[r]["gold"]) - {g}
                     and not [x for st in recs[r]["strays"] for x in st if x != g]]
            b = (b[0], b[1] + (f"; its record {right[0]} is right in substance (stage {v['gold_stage']})" if right
                               else f"; its records {last['span_records'] or 'none'} are not right either"))
        if stage_ok: fold.append((b[0], b[1] + "; the record's stage is right anyway"))
        else: reader.insert(0, b)
    elif not R or (len(R) == 1 and v["record_stages"][0] != v["gold_stage"]):
        if not set(last["records"]) & set(R):
            whose = [d for r in last["records"] for d in recs[r]["gold"] if d != g]
            mixed = {tuple(cl["about"].split("/", 1)) for cl in last["claims"] if D["unit_deals"].get(tuple(cl["about"].split("/", 1)), set()) - {g}}
            (reader if mixed else resolve).append(("in_window_merge" if mixed else "cross_window_merge" if whose else "cross_window_split",
                           f"deciding update {last['file']} is on {last['records'] or 'no record'}, the deal's record is {R or 'none'}"
                           + (f"; its particular also holds: {intruders(D, mixed, g)}" if mixed else "")))
        elif not merged:
            fold.append(("later_claim_in_record", f"deciding update {last['file']} ({last['stage']}) is on {R[0]}, whose stage is {v['record_stages'][0]}"))
    breaks = scorer + reader + resolve + fold
    if not breaks: breaks.append(("unclassified", "no rule fired"))
    mech, detail = breaks[0]
    # The referent of the update the verdict names: the deciding update's, or the first intruder's on a merged particular.
    m = re.search(r"\(referent (\w+)\)", detail)
    ref = m.group(1) if m else last["referent"]["where"] if detail.startswith("deciding update") and mech not in ("later_claim_in_record",) \
        and not mech.startswith("cross_window") else None
    deciding = detail.startswith("deciding update")
    root = ("hidden_referent" if mech in ("misattributed", "in_window_merge", "in_window_split") and ref == "quoted_only"
            else "end_unwritten" if deciding and last["stage"] == "lost" and mech in ("two_deal_message", "not_emitted", "other_sentence_cited")
            else ROOT.get(mech, "unclassified"))
    if root == "resolve":
        j = [x for c in chains for x in c["judged"] if x["verdict"] not in ("right_link", "right_open", "unjudged")]
        if j: detail += f"; RESOLVE: {', '.join(sorted({x['verdict'] + ('' if x.get('offered_right') is None else (' (own record offered)' if x['offered_right'] else ' (own record not offered)')) for x in j}))}"
    return dict(corpus=D["c"]["id"], deal=g, description=D["deals"][g].get("description", "")[:120], gold_stage=v["gold_stage"],
                records=[dict(id=r, stage=recs[r]["stage"], gold=recs[r]["gold"], strays=recs[r]["strays"]) for r in R],
                verdict=dict(kind=KIND.get(mech, "UNCLASSIFIED"), mechanism=mech, detail=detail, referent=ref, root=root),
                also=[dict(mechanism=m, detail=d) for m, d in breaks[1:]], updates=chains)


def oneshot_served(c):
    p = c.get("oneshot")
    if not p or not os.path.exists(p): return {}
    O = json.load(open(p))
    return {g: served(O, g) for g in O["gold"]}


if __name__ == "__main__":
    run, cid = sys.argv[1], sys.argv[2]
    D = load(run, cid); one = oneshot_served(D["c"])
    import decisions  # decisions.py imports this module; imported here, after it is whole
    for st, q in decisions.parse(run).items():
        v, _, extra = decisions.judge(D, st, q)
        D["judged"][(st.split("@")[0], "/".join(decisions.unit(st) or ("?", "?")))] = dict(verdict=v, **(extra or {}))
    rows = [dict(verdict(D, g), oneshot_served=one.get(g)) for g in D["S"]["gold"] if not served(D["S"], g)]
    with open(f"{run}/misses.jsonl", "w") as f:
        for r in rows: f.write(json.dumps(r) + "\n")
    print(f"{cid}: {len(D['S']['gold']) - len(rows)}/{len(D['S']['gold'])} served; {len(rows)} unserved; "
          f"one-shot serves {sum(1 for r in rows if r['oneshot_served'])} of them")
    for root, n in Counter(r["verdict"]["root"] for r in rows).most_common():
        print(f"  {n:>3}  {root:<16} one-shot serves {sum(1 for r in rows if r['verdict']['root'] == root and r['oneshot_served'])}")
    for r in rows:
        print(f"\n{r['deal']}  gold {r['gold_stage']}  one-shot {'served' if r['oneshot_served'] else 'not served' if r['oneshot_served'] is False else '?'}  -- {r['description']}")
        print(f"  ROOT    {r['verdict']['root']}")
        print(f"  VERDICT {r['verdict']['kind']}/{r['verdict']['mechanism']}" + (f" [referent {r['verdict']['referent']}]" if r['verdict']['referent'] else "") + f": {r['verdict']['detail']}")
        for a in r["also"]: print(f"  also    {a['mechanism']}: {a['detail']}")
        for rec in r["records"]: print(f"  record  {rec['id']} stage={rec['stage']} gold={rec['gold']} strays={rec['strays']}")
        for c in r["updates"]:
            print(f"  update  {c['file']} {c['date'][:16]} {c['stage']:<11} {c['window'] or 'in no window'} visible={c['visible']} -> {c['status']}  records={c['records']}")
            print(f"            gold: {c['quote'][:150]!r}")
            rf = c["referent"]; print(f"            referent {rf['where']}: tells it apart {rf['distinguishing']}; own lines {rf['own_lines']}, subject {rf['subject']}, raw {rf['raw']}")
            for cl in c["claims"]: print(f"            wrote {cl['kind']} {cl['stage']} about {cl['about']} ({cl['about_name']!r}, is {cl['particular_is']}){' REFUSED' if cl['refused'] else ''}: {cl['quote'][:110]!r}")
            for cl in c["other_statements_on_message"]: print(f"            same message: {cl['stage']} about {cl['about']} ({cl['about_name']!r}, is {cl['particular_is']}): {cl['quote'][:110]!r}")
            for cl in c["spans"]: print(f"            wrote a SPAN: {cl['stage']} about {cl['about']} ({cl['about_name']!r}, is {cl['particular_is']}): {cl['quote'][:110]!r}  records={c['span_records']}")
            for l in c["resolve"][:3]: print(f"            resolve: {l[:260]}")
            for x in c["judged"]: print(f"            judged:  {x['about']} {x['verdict']}" + ("" if x.get("offered_right") is None else f" (own record offered: {x['offered_right']})"))
            for l in c["thought"]: print(f"            thought: {l[:240]}")
            print(f"            one-shot: {'; '.join(c['oneshot'])[:300] if c['oneshot'] else 'wrote nothing for it'}")
