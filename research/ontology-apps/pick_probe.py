"""Can the reader fill a deal's counterparty from text? An offline Mention + Pick probe (ONTOLOGY_METHOD §Reading;
order ontology-layer-14-pick-probe), over blind round 2's Ward run.

    python3 pick_probe.py join    # pick-probe/join.json: the deals, their Pick message, header candidates; no call
    python3 pick_probe.py run     # NER + Mention + Pick per Pick message, cached under runs/pick-probe/calls/
    python3 pick_probe.py score   # pick-probe/score.json and rows.jsonl, from the cache only

THE JOIN (no call). The blind run's atlas through ward_placing.load (ladder.atlas_view -> ladder.translate with
blind-author/r2-mapping.json, then ward/score.py load_atlas, which gives each claim its gold files `_files`). A blind
DEAL CLAIM is a claim whose subject is a `deal` record (deal_step, read as stage_update, and blind:deal_terms). The
deals are order 12's rows (ward-placing/e4-ward-tune.json: the 47 tune gold deals with a current stage, each with its
class); a deal is in the probe when some blind deal claim's `_files` meets the deal's gold files. Its PICK MESSAGE is
the latest (score.when) of those files; its CLAIM is the deal claim on that file, stage_update before deal_terms, then
lowest claim id; the claim's cited lines are its quotable_excerpt. One Pick per message (a message several deals share
is asked once, so of the deals sharing it at most those with one counterparty can be right; counted). Gold deals outside order 12's rows that a blind deal claim reaches are counted, not scored.

THE DOCUMENT: the run's own stored text for the message (data/indexes/crm-ward/documents.lance -> texts/<sha>), the
body the reader read, headers included; cut at MAX_DOC characters (counted).

CANDIDATES (code proposes; ONTOLOGY_METHOD Pick). HEADER: each address domain in From, To, Cc, in that order, that is
the domain of a company record of the run (the blind recipe's company source: metadata from/to/cc, identity domain),
shown as the record's canonical name. The blind recipe declares no exclusion set for counterparty, so none is applied
(its own domain included). MENTIONS, two proposers, each its own arm:
  (a) NER: POST /v1/ner over the document cut into paragraphs (blank-line runs, at most NER_CHUNK characters), label
      Organization, one request per message;
  (b) MENTION: one chat call over the whole document: the declared type the reference targets (company: its name and
      description, no other word of ours), asking for every span that names one particular company, verbatim.
Code verifies every span: an NER span must equal its chunk's characters at its offsets, a chat span must occur in the
document verbatim; an unverified span is refused and counted. Verified spans are deduplicated by score.fold of their
text, in document order, and appended after the header candidates; past 25 candidates (the reader's labels A-Y) the
rest are cut and counted.

THE PICK QUESTION: the reader's Choose form (resolve_records/read.rs choice_question; passes.rs): the declared type
(deal: name, description), the attribute (counterparty, a reference to company: the target's name and description;
the attribute declares none of its own), the document, the claim's cited lines, then the candidates each under a
letter and 0 for none. Forced choice: response_format json_schema {"type":"string","enum":labels,
"x_forced_choice":true} (oicp_types::forced_choice::schema), answered as a distribution; temperature 0, thinking off.
The answer is the argmax; 0 is "none".

MATCHING GOLD (the forms ward_placing.py and ward/score.py use). A HEADER candidate is gold's counterparty when its
domain equals one of gold's domains or is a subdomain of one (ward_placing.is_gold), or score.company_resolver's
`resolves` says its record is that gold company. A MENTION candidate is gold's when ward_placing.names(span) is
non-empty: one of gold's written forms (score.gold_forms: the name and each parenthetical alias, through name_core) is
a whole-word run of the folded span, or a gold domain is a substring of the lowercased span.
  right          the argmax candidate is gold's counterparty
  wrong-company  the argmax is a candidate that is not
  none           the argmax is 0
  failed         the call failed or answered no distribution (never-ran for that unit, not "none")
  candidate recall  some candidate (header or the arm's mentions) is gold's counterparty
NAMED IN TEXT: ward_placing.names over the Pick message's document text with its address lines (ward_placing
ADDRESS_LINE) removed is non-empty.

WORTH A PASS (pre-registered, committed before any scoring call; the order's suggested rule, unsharpened). Per arm,
over the deals whose counterparty is NAMED IN TEXT: right >= .60 AND wrong-company <= .15 AND daemon calls per message
<= 3 (NER, Mention and Pick each count as one). An arm meeting all three is "worth a pass"; Mention and Pick are worth
building when at least one arm is. If more than 10% of an arm's calls fail, or fewer than 10 deals are NAMED IN TEXT,
that arm is could-not-judge, not a fail.
"""
import collections, hashlib, json, pathlib, re, sys, time, tomllib, urllib.error, urllib.request

import ward_placing as P

S = P.S
HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
RUN = ROOT / "runs/blind-r2/ward-tune"
ROWS12 = HERE / "ward-placing/e4-ward-tune.json"  # order 12's rows and classes
OUT = ROOT / "runs/pick-probe"  # the call cache (gitignored, as runs/ is)
DATA = HERE / "pick-probe"  # committed join, scores and rows
CHAT, NER, MODEL = "http://localhost:9741/v1/chat/completions", "http://localhost:9741/v1/ner", "commonwealth/primary"
LABELS, NONE = [chr(c) for c in range(ord("A"), ord("Y") + 1)], "0"  # resolve_records/select.rs
MAX_DOC, NER_CHUNK = 20000, 1200
DEAL_KINDS = ("stage_update", "blind:deal_terms")
BARS = {"right": 0.60, "wrong_company": 0.15, "calls_per_message": 3}
MAX_FAILED, MIN_NAMED = 0.10, 10

MENTION_SYSTEM = ("You read a document and point at each place in it that names a particular {name}.\n\n"
                  "The user message gives the declared type with what it means, and the document. Answer with every "
                  "span of the document that names one particular {name}, each copied exactly as it is written, in "
                  "the order they appear. Answer with an empty list if it names none.")
MENTION_SCHEMA = {"type": "object", "additionalProperties": False, "required": ["spans"],
                  "properties": {"spans": {"type": "array", "maxItems": 32, "items": {"type": "string"}}}}
PICK_SYSTEM = ("You read one statement of a document and say which of the candidates a declared reference attribute "
               "refers to.\n\nThe user message gives a declared type, one of its attributes and the type it refers "
               "to, the document, the statement's cited lines, and the candidates, each under a letter. Answer with "
               "the letter of the candidate the attribute of the particular the statement is about refers to, or 0 "
               "if none of them fits or the document does not say.")


# ---------------------------------------------------------------- the declaration (the blind recipe's words only)
def declaration():
    recipe = tomllib.loads(pathlib.Path(P.L.run_recipe(RUN)).read_text())
    types = {t["name"]: t for t in recipe["enrichment"]["ontology"]["types"]}
    deal = types["deal"]
    attr = next(a for a in deal["attributes"] if a["name"] == "counterparty")
    target = types[attr["of"]]
    return {"type": deal["name"], "type_description": deal.get("description", ""), "attribute": attr["name"],
            "attribute_description": attr.get("description", ""), "target": target["name"],
            "target_description": target.get("description", "")}


# ---------------------------------------------------------------- the join (step 1, no call)
def documents():
    import lance  # noqa: PLC0415
    idx = RUN / "data/indexes/crm-ward"
    rows = lance.dataset(str(idx / "documents.lance")).to_table(columns=["source_id", "text_sha256", "metadata"]).to_pylist()
    return {r["source_id"]: {"meta": json.loads(r["metadata"] or "{}"), "text": (idx / "texts" / r["text_sha256"]).read_text(errors="replace")}
            for r in rows}


def header_candidates(meta, ent):
    by_domain = {}
    for e in ent.values():
        if e.get("entity_type") == "company":
            for d in S.attr_list(e, "domain"):
                by_domain.setdefault(S.fold(d), e["id"])
    out = []
    for field in ("from", "to", "cc"):
        for _, addr in P.email.utils.getaddresses([meta.get(field) or ""]):
            d = P.domain_of(addr)
            if d and S.fold(d) in by_domain and not any(c["domain"] == S.fold(d) for c in out):
                atom = by_domain[S.fold(d)]
                out.append({"source": "header", "domain": S.fold(d), "atom": atom, "text": ent[atom]["canonical_name"]})
    return out


def join():
    X = P.load(RUN)
    if "ladder" not in X:
        sys.exit(f"the blind run does not load: {X}")
    rows12 = {r["deal"]: r for r in json.loads(ROWS12.read_text())["rows"]}
    ent, claims, g = X["ent"], X["claims"], X["g"]
    deal_claims = [c for c in claims if (ent.get(c.get("subject")) or {}).get("entity_type") == "deal"]
    path_of = {m: e["path"] for e in json.loads((S.WARD / "manifest.json").read_text()) for m in e["message_ids"]}
    docs = documents()
    doc_of_file = {path_of[d["meta"]["message_id"]]: sid for sid, d in docs.items() if d["meta"].get("message_id") in path_of}
    atoms_of, resolves = S.company_resolver(g, ent)
    deals, outside = [], []
    for d in g["deals"]:
        on = [c for c in deal_claims if c["_files"] & d["files"]]
        if not on:
            continue
        if d["id"] not in rows12:
            outside.append(d["id"])
            continue
        f = max({x for c in on for x in c["_files"] & d["files"]}, key=S.when)
        here = sorted((c for c in on if f in c["_files"]),
                      key=lambda c: (DEAL_KINDS.index(c["claim_kind"]) if c["claim_kind"] in DEAL_KINDS else 9, c["id"]))
        c = here[0]
        sid = doc_of_file.get(f)
        if sid is None:
            sys.exit(f"{d['id']}: its Pick message {f} has no document in the run's index")
        cp = next(x for x in g["companies"] if x["id"] == d["counterparty"])
        gold_domains = [S.fold(x) for x in cp.get("domains") or []]
        forms = sorted(S.gold_forms(cp))
        text = docs[sid]["text"]
        body = "\n".join(l for l in text.splitlines() if not P.ADDRESS_LINE.match(l.strip()))
        heads = header_candidates(docs[sid]["meta"], ent)
        for h in heads:
            h["gold"] = P.is_gold(h["domain"], gold_domains) or resolves(h["atom"], d["counterparty"])
        deals.append({"deal": d["id"], "class12": rows12[d["id"]]["class"], "counterparty": cp["name"],
                      "gold_id": d["counterparty"], "gold_domains": gold_domains, "forms": forms,
                      "deal_messages": len(d["files"]), "messages_with_deal_claim": len({x for c in on for x in c["_files"] & d["files"]}),
                      "pick_file": f, "pick_is_latest": any(m["file"] == f and m["latest"] for m in rows12[d["id"]]["messages"]),
                      "document": sid, "claim": c["id"], "claim_kind": c["claim_kind"],
                      "cited": c.get("quotable_excerpt") or c.get("content") or "",
                      "named_in_text": P.names(body, forms, gold_domains),
                      "header": heads, "header_has_gold": any(h["gold"] for h in heads)})
    return {"run": str(RUN), "rows12": str(ROWS12), "declaration": declaration(),
            "deal_claims": dict(collections.Counter(c["claim_kind"] for c in deal_claims)),
            "deals_in_order12": len(rows12), "deals_probed": len(deals),
            "deals_without_deal_claim": sorted(set(rows12) - {x["deal"] for x in deals}),
            "gold_deals_outside_order12_reached": sorted(outside),
            "messages": len({x["document"] for x in deals}),
            "classes": dict(collections.Counter(x["class12"] for x in deals)),
            "deals_sharing_their_pick_message": sum(n for n in collections.Counter(x["document"] for x in deals).values() if n > 1),
            "named_in_text": sum(bool(x["named_in_text"]) for x in deals), "deals": deals}, docs


# ---------------------------------------------------------------- the questions
def chat_body(system, user, schema_name, schema, max_tokens):
    return {"model": MODEL, "temperature": 0, "max_tokens": max_tokens, "think_budget": 0,
            "thinking": {"type": "disabled"}, "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "response_format": {"type": "json_schema", "json_schema": {"name": schema_name, "strict": True, "schema": schema}}}


def mention_body(decl, doc):
    user = (f"Type: {decl['target']} ({decl['target_description']})\n\nDocument:\n<<<\n{doc}\n>>>\n\n"
            f"Which spans of the document each name one particular {decl['target']}?")
    return chat_body(MENTION_SYSTEM.format(name=decl["target"]), user, "mentions", MENTION_SCHEMA, 768)


def pick_body(decl, doc, cited, candidates):
    labels = LABELS[:len(candidates)] + [NONE]
    attr = f"Attribute: {decl['attribute']}"
    if decl["attribute_description"]:
        attr += f" ({decl['attribute_description']})"
    attr += f", a {decl['target']} ({decl['target_description']})"
    u = (f"Type: {decl['type']} ({decl['type_description']})\n{attr}\n\nDocument:\n<<<\n{doc}\n>>>\n\n"
         f"Statement, its cited lines: \"{' '.join(cited.split())}\"\n\n"
         f"Which {decl['target']} is the {decl['attribute']} of the {decl['type']} the statement is about?\n")
    for c, l in zip(candidates, labels):
        u += f"{l} {c['text']}\n"
    u += f"{NONE} none of them, or the document does not say\nAnswer with its letter."
    return chat_body(PICK_SYSTEM, u, "read", {"type": "string", "enum": labels, "x_forced_choice": True}, 16), labels


def ner_chunks(doc):
    """Paragraphs (blank-line runs) with their document offsets, a paragraph longer than NER_CHUNK cut at a space,
    consecutive pieces packed while they fit in NER_CHUNK characters."""
    pieces, start = [], 0
    for m in re.finditer(r"\n\s*\n|\Z", doc):
        while m.start() - start > NER_CHUNK:
            cut = doc.rfind(" ", start + 1, start + NER_CHUNK)
            cut = cut if cut > start else start + NER_CHUNK
            pieces.append((start, cut))
            start = cut
        if doc[start:m.start()].strip():
            pieces.append((start, m.start()))
        start = m.end()
    packed = []
    for s, e in pieces:
        if packed and e - packed[-1][0] <= NER_CHUNK:
            packed[-1] = (packed[-1][0], e)
        else:
            packed.append((s, e))
    return [(s, doc[s:e]) for s, e in packed]


def cache_path(kind, body):
    return OUT / "calls" / kind / f"{hashlib.sha1(json.dumps(body, sort_keys=True).encode()).hexdigest()[:16]}.json"


def post(url, kind, body, call):
    """One call, cached by its whole input: (record or None when not cached and call is False, cached)."""
    path = cache_path(kind, body)
    if path.exists():
        return json.loads(path.read_text()), True
    if not call:
        return None, False
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})
    t0, rec, resp = time.time(), {"request": body}, None
    for attempt in range(20):  # the daemon sheds past its queue budget with 503 + Retry-After: honour it
        try:
            with urllib.request.urlopen(req, timeout=900) as r:
                resp = json.loads(r.read())
            break
        except urllib.error.HTTPError as e:
            if e.code != 503 or attempt == 19:
                rec["error"] = f"HTTP {e.code}: {e.read()[:200]!r}"
                break
            time.sleep(float(e.headers.get("Retry-After") or 15))
        except (urllib.error.URLError, TimeoutError) as e:
            rec["error"] = f"{type(e).__name__}: {e}"
            break
    rec.update(wall_s=round(time.time() - t0, 2), response=resp)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(rec, indent=1))
    return rec, False


# ---------------------------------------------------------------- verification
def ner_mentions(rec, chunks):
    kept, refused = [], 0
    for (off, text), ms in zip(chunks, ((rec or {}).get("response") or {}).get("mentions") or []):
        for m in ms:
            if m.get("label") != "Organization":
                continue
            s, e = m.get("char_start"), m.get("char_end")
            # offsets may be characters (as Python counts them): verify against the chunk's own text
            if isinstance(s, int) and isinstance(e, int) and text[s:e] == m.get("text"):
                kept.append((off + s, m["text"]))
            elif m.get("text") and m["text"] in text:
                kept.append((off + text.index(m["text"]), m["text"]))
            else:
                refused += 1
    return kept, refused


def chat_mentions(rec, doc):
    kept, refused = [], 0
    content = ((((rec or {}).get("response") or {}).get("choices") or [{}])[0].get("message") or {}).get("content") or ""
    try:
        spans = json.loads(content).get("spans") or []
    except (json.JSONDecodeError, AttributeError):
        return None, 0
    for sp in spans:
        i = doc.find(sp) if isinstance(sp, str) and sp.strip() else -1
        if i < 0:
            refused += 1
        else:
            kept.append((i, sp))
    return kept, refused


def candidates(header, mentions):
    """header first, then verified mentions in document order, deduplicated by folded text; (list, cut)."""
    out, seen = list(header), {S.fold(h["text"]) for h in header}
    for at, text in sorted(mentions):
        k = S.fold(text)
        if k and k not in seen:
            seen.add(k)
            out.append({"source": "mention", "text": text, "at": at})
    return out[:len(LABELS)], max(0, len(out) - len(LABELS))


def is_gold_candidate(c, d):
    if c["source"] == "header":
        return c["gold"]
    return bool(P.names(c["text"], d["forms"], d["gold_domains"]))


def distribution(rec, labels):
    content = ((((rec or {}).get("response") or {}).get("choices") or [{}])[0].get("message") or {}).get("content") or ""
    try:
        dist = json.loads(content)
    except json.JSONDecodeError:
        return None
    return dist if isinstance(dist, dict) and all(l in dist for l in labels) else None


def usage(rec):
    u = ((rec or {}).get("response") or {}).get("usage") or {}
    return u.get("prompt_tokens", 0), u.get("completion_tokens", 0)


# ---------------------------------------------------------------- the probe
def probe(call):
    J, docs = join()
    decl = J["declaration"]
    per_msg = {}
    for d in J["deals"]:
        if d["document"] in per_msg:
            continue
        doc = docs[d["document"]]["text"]
        cut = len(doc) > MAX_DOC
        doc = doc[:MAX_DOC]
        chunks = ner_chunks(doc)
        ner_rec, _ = post(NER, "ner", {"texts": [t for _, t in chunks]}, call)
        men_rec, _ = post(CHAT, "mention", mention_body(decl, doc), call)
        arms = {}
        a_spans, a_ref = ner_mentions(ner_rec, chunks) if ner_rec and not ner_rec.get("error") else (None, 0)
        b_spans, b_ref = chat_mentions(men_rec, doc) if men_rec and not men_rec.get("error") else (None, 0)
        for arm, spans, refused, rec in (("ner", a_spans, a_ref, ner_rec), ("chat", b_spans, b_ref, men_rec)):
            if spans is None:
                arms[arm] = {"proposer_failed": rec is not None, "never_ran": rec is None}
                continue
            cands, over = candidates(d["header"], spans)
            body, labels = pick_body(decl, doc, d["cited"], cands)
            prec, _ = post(CHAT, "pick", body, call)
            arms[arm] = {"spans": len(spans), "refused": refused, "candidates": cands, "cut": over, "labels": labels,
                         "pick": prec, "dist": distribution(prec, labels)}
        per_msg[d["document"]] = {"doc_cut": cut, "chunks": len(chunks), "ner": ner_rec, "mention": men_rec, "arms": arms}
    return J, per_msg


def score():
    J, per_msg = probe(call=False)
    rows, tally = [], {}
    for d in J["deals"]:
        m = per_msg[d["document"]]
        r = {k: d[k] for k in ("deal", "class12", "counterparty", "pick_file", "pick_is_latest", "claim", "claim_kind",
                                "named_in_text", "header_has_gold")}
        r["header"] = [h["text"] for h in d["header"]]
        for arm, a in m["arms"].items():
            if "dist" not in a:
                r[arm] = {"outcome": "failed"}
                continue
            cands = a["candidates"]
            recall = any(is_gold_candidate(c, d) for c in cands)
            mention_recall = any(is_gold_candidate(c, d) for c in cands if c["source"] == "mention")
            if a["dist"] is None:
                out, best = "failed", None
            else:
                best = max(a["labels"], key=lambda l: a["dist"][l])
                if best == NONE:
                    out = "none"
                else:
                    c = cands[a["labels"].index(best)]
                    out = "right" if is_gold_candidate(c, d) else "wrong_company"
            pick = cands[a["labels"].index(best)] if best not in (None, NONE) else None
            r[arm] = {"outcome": out, "picked": pick and pick["text"], "picked_source": pick and pick["source"],
                      "p": round(a["dist"][best], 3) if best else None, "candidate_recall": recall,
                      "mention_recall": mention_recall, "candidates": len(cands), "mentions": a["spans"],
                      "refused": a["refused"], "cut": a["cut"]}
        rows.append(r)

    def share(sub, arm):
        n = len(sub)
        c = collections.Counter(x[arm]["outcome"] for x in sub)
        f = lambda k: round(c[k] / n, 3) if n else None  # noqa: E731
        return {"n": n, "right": f("right"), "wrong_company": f("wrong_company"), "none": f("none"),
                "failed": c["failed"], "candidate_recall": round(sum(x[arm].get("candidate_recall", False) for x in sub) / n, 3) if n else None,
                "mention_recall": round(sum(x[arm].get("mention_recall", False) for x in sub) / n, 3) if n else None}

    cost = {}
    for arm, prop in (("ner", "ner"), ("chat", "mention")):
        calls = fails = pt = ct = 0
        for m in per_msg.values():
            calls += 1 + ("pick" in m["arms"][arm] and m["arms"][arm]["pick"] is not None)
            fails += bool((m[prop] or {}).get("error")) + bool((m["arms"][arm].get("pick") or {}).get("error"))
            for rec in (m[prop] if prop == "mention" else None, m["arms"][arm].get("pick")):
                a, b = usage(rec)
                pt, ct = pt + a, ct + b
        n = len(per_msg)
        cost[arm] = {"messages": n, "calls": calls, "calls_per_message": round(calls / n, 2), "failed_calls": fails,
                     "chat_prompt_tokens_per_message": round(pt / n), "chat_completion_tokens_per_message": round(ct / n)}
    named = [r for r in rows if r["named_in_text"]]
    verdict = {}
    for arm in ("ner", "chat"):
        s = share(named, arm)
        if cost[arm]["failed_calls"] > MAX_FAILED * cost[arm]["calls"] or s["n"] < MIN_NAMED:
            verdict[arm] = {"verdict": "could-not-judge", **s}
            continue
        ok = {"right": s["right"] >= BARS["right"], "wrong_company": s["wrong_company"] <= BARS["wrong_company"],
              "calls_per_message": cost[arm]["calls_per_message"] <= BARS["calls_per_message"]}
        verdict[arm] = {"verdict": "worth a pass" if all(ok.values()) else "not worth a pass", "bars": ok, **s}
    out = {"join": {k: v for k, v in J.items() if k != "deals"}, "bars": BARS, "cost": cost,
           "named_in_text": {arm: share(named, arm) for arm in ("ner", "chat")},
           "all": {arm: share(rows, arm) for arm in ("ner", "chat")},
           "by_class": {cl: {arm: share([r for r in rows if r["class12"] == cl], arm) for arm in ("ner", "chat")}
                        for cl in sorted({r["class12"] for r in rows})},
           "by_class_named": {cl: {arm: share([r for r in named if r["class12"] == cl], arm) for arm in ("ner", "chat")}
                              for cl in sorted({r["class12"] for r in named})},
           "header_has_gold": sum(r["header_has_gold"] for r in rows),
           "verdict": verdict,
           "worth_building": any(v["verdict"] == "worth a pass" for v in verdict.values())}
    DATA.mkdir(exist_ok=True)
    (DATA / "score.json").write_text(json.dumps(out, indent=1) + "\n")
    (DATA / "rows.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
    print(json.dumps({k: out[k] for k in ("cost", "named_in_text", "all", "by_class", "verdict", "worth_building")}, indent=1))


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "join"
    if cmd == "join":
        J, _ = join()
        DATA.mkdir(exist_ok=True)
        (DATA / "join.json").write_text(json.dumps(J, indent=1, default=sorted) + "\n")
        print(json.dumps({k: v for k, v in J.items() if k != "deals"}, indent=1, default=sorted))
    elif cmd == "run":
        J, per = probe(call=True)
        print(f"{len(per)} messages asked")
    elif cmd == "score":
        score()
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
