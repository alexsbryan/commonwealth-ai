"""Locate probe (2026-10-12): does another question shape recover the facts a line implies, on all three systems,
without losing the lines read right or flagging more unmarked lines? Bars: locate-probe/PREREG.md (written first).

    python3 locate_probe.py join              # population, no call: runs/locate-probe/join.json (documents
                                             # included, so not committed) and locate-probe/population.json
    python3 locate_probe.py run ARM [ARM..]   # C V Y T; calls cached by request under runs/locate-probe/calls/
    python3 locate_probe.py score             # from the cache only: locate-probe/score.json, rows.jsonl

THE VIEW. The reader's own, rebuilt from the leg's trace: every line of a read document is logged (located or not
asked, with its text), so the numbered document is the trace's lines in order; facts are prefill.rs's, from the
chunk metadata. Leading quote marks the reader keeps in its numbered view are not in the trace: the gate (C against
the logged distribution) measures what that costs before any arm is read.
"""
import collections, hashlib, json, math, pathlib, random, re, sys, time, tomllib, urllib.error, urllib.request

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import ladder as L  # noqa: E402

RUNS = ROOT / "runs/whole-text"
OUT = ROOT / "runs/locate-probe"  # the call cache (runs/ is gitignored)
DATA = HERE / "locate-probe"
READER = ROOT / "ingest/crates/corpus-engine/src/enrichment/pipeline/document_read"
CHAT, MODEL = "http://localhost:9741/v1/chat/completions", "commonwealth/primary"
LABELS, NONE = [chr(c) for c in range(ord("A"), ord("Y") + 1)], "0"  # resolve_records/select.rs
SEED = 20261012
GVC_CAP = {"lost": 100, "hit": 50}
SYSTEMS = {"ward": ("ours-ward", "crm-ward"), "uv": ("ours-uv", "uv-support"), "gvc": ("ours-gvc", "cdcr-gvc")}
HEADER_LINE = re.compile(r"^\s*(From|To|Cc|Bcc|Sent|Subject|Date|Re|Fw|Fwd)\s*:", re.I)
SENTENCE_END = re.compile(r"""[.?!:;"')\]]\s*$""")
REFERENCE = {"it", "this", "that", "these", "those", "they", "them", "which", "what", "its", "one"}

LOCATE_SYSTEM = (READER / "passes_locate_prompt.md").read_text()
LOCATE_SHOWS_VALUES = ", followed by the values its fields can take"  # passes.rs:55
FACTS_HEADER = "Declared facts of this document:\n"  # prefill.rs
V_SYSTEM = ("You read one line of a document and say which declared kind of claim it states, if any, and with which "
            "value.\n\nThe user message gives the document with its lines numbered, then one of its lines and the "
            "declared kinds of claim with what each means; a kind with a closed field has one letter per value, a kind "
            "without one has one letter. Answer with the letter of the kind and value the line states or clearly "
            "implies, or 0 if it states none of them.")
Y_SYSTEM = ("You read one line of a document and say whether it states one declared kind of claim.\n\nThe user message "
            "gives the document with its lines numbered, then one of its lines and the declared kind with what it "
            "means{values}. Answer A if the line states or clearly implies a claim of that kind, or 0 if it does not.")
T_SYSTEM = ("You read one line of a document and say whether it is about a particular of one declared type.\n\nThe user "
            "message gives the document with its lines numbered, then one of its lines and the declared type with what "
            "it means. Answer A if the line is about one particular of that type, or 0 if it is not.")

# ---------------------------------------------------------------- the leg's trace
ANSI = re.compile(r"\x1b\[[0-9;]*m")
LOC = re.compile(r'document_read/passes: located document=(\S+) line=(\d+) kind="([^"]+)" p=([\d.]+) dist=(.*?) text=(.*)$')
NOT = re.compile(r'document_read/passes: line not asked document=(\S+) line=(\d+) class="([^"]+)" from="[^"]+" text=(.*)$')
CHO = re.compile(r'document_read/passes: chose(?: none of the values)? document="(\S+)" statement="l(\d+)-(\d+)" field=(\w+) (?:value=(\S+) )?p=')


def dist_of(text):
    return {k: float(v) for k, v in (x.rsplit(" ", 1) for x in text.split(", "))}


def trace(leg):
    lines, chose = collections.defaultdict(dict), collections.defaultdict(list)
    for raw in open(RUNS / leg / "extract.err.log", errors="replace"):
        raw = ANSI.sub("", raw).rstrip("\n")
        if m := LOC.search(raw):
            lines[m[1]][int(m[2])] = {"asked": True, "kind": m[3], "dist": dist_of(m[5]), "text": m[6]}
        elif m := NOT.search(raw):
            lines[m[1]][int(m[2])] = {"asked": False, "class": m[3], "text": m[4]}
        elif m := CHO.search(raw):
            chose[m[1]].append((int(m[2]), int(m[3]), m[4], m[5] or NONE))
    return lines, chose


def norm(s):
    return re.sub(r"[^a-z0-9]+", " ", s.lower()).strip()


def find(doc_lines, quote):
    """The line holding a gold quote: the longest line contained in it, or holding its opening."""
    q, best = norm(quote), None
    for n, ln in doc_lines.items():
        t = norm(ln["text"])
        if len(t) >= 6 and (t in q or q[:60] in t or (len(q) > 30 and q[:30] in t)):
            if best is None or len(t) > len(norm(doc_lines[best]["text"])):
                best = n
    return best


# ---------------------------------------------------------------- the declaration and the facts (prefill.rs)
def recipe(leg, corpus):
    return tomllib.loads((RUNS / leg / f"data/recipes/{corpus}/recipe.toml").read_text())


def plan(rec):
    """Claim kinds in declaration order, each with its closed fields (passes.rs FieldPlan::Choose), and the decided
    subject types (an identity criterion, no source)."""
    types = rec["enrichment"]["ontology"]["types"]
    by = {t["name"]: t for t in types}
    kinds = []
    for t in types:
        if t.get("kind") != "claim":
            continue
        closed = [a for a in t.get("attributes", []) if a.get("values") and not a.get("derived")
                  and a.get("type", "text") == "text" and len(a["values"]) <= 25]
        kinds.append({"name": t["name"], "description": t.get("description", ""), "fields": closed,
                      "subject": t.get("subject")})
    subjects = []
    for k in kinds:
        s = by.get(k["subject"]) or {}
        if s.get("identity_criterion") and not s.get("source") and s["name"] not in [x["name"] for x in subjects]:
            subjects.append({"name": s["name"], "description": s.get("description", "")})
    return kinds, subjects


def strings(v):
    if isinstance(v, str):
        return [v]
    if isinstance(v, (int, float)) and not isinstance(v, bool):
        return [str(v)]
    if isinstance(v, list):
        return [x for e in v for x in strings(e)]
    if isinstance(v, dict):
        return [x for e in v.values() for x in strings(e)]
    return []


def holds(where, attrs):
    for k, cond in where.items():
        v = attrs.get(k)
        if v is None:
            return False
        vs = strings(v)
        if isinstance(cond, list):
            if not any(x in cond for x in vs):
                return False
        elif isinstance(cond, dict) and "suffix" in cond:
            if not any(x.lower().endswith(cond["suffix"].lower()) for x in vs):
                return False
        elif cond not in vs:
            return False
    return True


def facts(rec, meta):
    ont = rec["enrichment"]["ontology"]
    change = (ont.get("change") or {}).get("document") or {}
    fields = []
    for t in ont["types"]:
        for f in ((t.get("source") or {}).get("metadata") or []):
            fields.append(f)
    fields += [change[s] for s in ("date", "thread", "id") if change.get(s)] + ([change["author"]] if change.get("author") else [])
    seen, out = set(), []
    for f in fields:
        if f in seen:
            continue
        seen.add(f)
        value = ", ".join(strings(meta.get(f))).strip() if f in meta else ""
        if value:
            out.append(f"{f}: {value}")
    by = {t["name"]: t for t in ont["types"]}
    for s in ont.get("sets") or []:
        if s.get("type") == "document":
            if holds(s.get("where") or {}, meta):
                out.append(f"this document is in `{s['id']}`")
            continue
        t = by.get(s.get("type")) or {}
        src = t.get("source") or {}
        reader = (src.get("attributes") or {})
        for f in src.get("metadata") or []:
            for addr in re.findall(r"[\w.+'-]+@[\w-]+(?:\.[\w-]+)+", " ".join(strings(meta.get(f)))):
                if "domain" in reader:
                    rec_attrs = {"domain": addr.split("@", 1)[1].lower()}
                    line = f"{f} names {t['name']} {rec_attrs['domain']}, which is in `{s['id']}`"
                    if holds(s.get("where") or {}, rec_attrs) and line not in out:
                        out.append(line)
    return f"{FACTS_HEADER}" + "\n".join(out) + "\n\n" if out else ""


def documents(leg, corpus):
    """{document: (metadata, raw lines)}: the reader's body is the document's chunks in order joined by newlines,
    and its lines are the non-blank ones (checked against every traced line of all three legs: no mismatch)."""
    import lance  # noqa: PLC0415
    ds = lance.dataset(str(RUNS / leg / f"data/indexes/{corpus}/chunks.lance"))
    by = collections.defaultdict(list)
    for r in ds.to_table(columns=["id", "source_doc_id", "metadata", "content"]).to_pylist():
        by[r["source_doc_id"]].append(r)
    out = {}
    for d, chunks in by.items():
        chunks.sort(key=lambda r: int(r["id"]))
        raw = [l.rstrip() for l in "\n".join(c["content"] for c in chunks).split("\n") if l.strip()]
        out[d] = (json.loads(chunks[0]["metadata"] or "{}"), raw)
    return out


def line_text(traced, raw):
    """The line as the question quotes it (body[start..end]): the trace's text, which the log cuts near 160
    characters; a cut one is completed from the raw line, from where the traced text starts."""
    if len(traced) < 150:
        return traced
    i = raw.find(traced[:40])
    return raw[i:].strip() if i >= 0 else raw.strip()


# ---------------------------------------------------------------- the questions
def numbered(raw):
    return "Document, its lines numbered:\n<<<\n" + "".join(f"{n} {l}\n" for n, l in enumerate(raw, 1)) + ">>>"


def head(view, n, text):
    return f"{view}\n\nLine {n}: \"{text}\"\n\n"


def field_lines(k):
    out = ""
    for a in k["fields"]:
        out += f"   {a['name']}: {', '.join(a['values'])}" + (f" ({a['description']})" if a.get("description") else "") + "\n"
    return out


def q_control(view, n, text, kinds):
    u = head(view, n, text) + f"Which kind of claim does line {n} state?\n"
    labels = LABELS[:len(kinds)]
    for k, l in zip(kinds, labels):
        u += f"{l} {k['name']}" + (f" ({k['description']})" if k["description"] else "") + "\n" + field_lines(k)
    u += f"{NONE} none of them\nAnswer with its letter."
    system = LOCATE_SYSTEM.replace("{values}", LOCATE_SHOWS_VALUES if any(k["fields"] for k in kinds) else "")
    return system, u, labels + [NONE], {l: (k["name"], None) for k, l in zip(kinds, labels)}


def q_values(view, n, text, kinds):
    u = head(view, n, text) + f"Which kind of claim does line {n} state, and with which value?\n"
    i, meaning = 0, {}
    for k in kinds:
        desc = f" ({k['description']})" if k["description"] else ""
        if k["fields"]:
            a = k["fields"][0]
            u += f"{k['name']}{desc}\n   {a['name']}" + (f" ({a['description']})" if a.get("description") else "") + "\n"
            for v in a["values"]:
                u += f"{LABELS[i]} {k['name']}: {a['name']} {v}\n"
                meaning[LABELS[i]] = (k["name"], v)
                i += 1
        else:
            u += f"{LABELS[i]} {k['name']}{desc}\n"
            meaning[LABELS[i]] = (k["name"], None)
            i += 1
    u += f"{NONE} none of them\nAnswer with its letter."
    return V_SYSTEM, u, LABELS[:i] + [NONE], meaning


def q_yes(view, n, text, k):
    u = head(view, n, text) + f"Does line {n} state a claim of this kind?\n{k['name']}" + (f" ({k['description']})" if k["description"] else "") + "\n" + field_lines(k)
    u += f"A yes\n{NONE} no\nAnswer with its letter."
    return Y_SYSTEM.replace("{values}", LOCATE_SHOWS_VALUES if k["fields"] else ""), u, ["A", NONE]


def q_about(view, n, text, s):
    u = head(view, n, text) + f"Is line {n} about a particular {s['name']}?\n{s['name']}" + (f" ({s['description']})" if s["description"] else "") + "\n"
    u += f"A yes\n{NONE} no\nAnswer with its letter."
    return T_SYSTEM, u, ["A", NONE]


def unit(raw, n):
    """What the asked line is as a unit of meaning (a measurement of the example, never a reader rule): a header
    line, a fragment (starts or stops mid-sentence), short (under six words), a paragraph (three or more sentences),
    or a sentence. Added after the PREREG for reporting only (ec's finding 9fba6f1e: Ward lines are wrap
    fragments); no bar reads it."""
    t = raw[n - 1].strip()
    prev, nxt = (raw[n - 2].strip() if n >= 2 else ""), (raw[n].strip() if n < len(raw) else "")
    if HEADER_LINE.match(t):
        return "header"
    if (prev and not SENTENCE_END.search(prev) and t[:1].islower()) or (nxt and not SENTENCE_END.search(t) and nxt[:1].islower()):
        return "fragment"
    if len(t.split()) < 6:
        return "short"
    return "paragraph" if len(re.findall(r"[.?!](\s|$)", t)) >= 3 else "sentence"


def body(system, user, labels):
    return {"model": MODEL, "temperature": 0, "max_tokens": 16, "think_budget": 0, "thinking": {"type": "disabled"},
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "response_format": {"type": "json_schema", "json_schema": {"name": "read", "strict": True,
                                "schema": {"type": "string", "enum": labels, "x_forced_choice": True}}}}


# ---------------------------------------------------------------- the population (no call)
def gold_lines(system, lines):
    """{(doc, line): (kind, value)} for the system's gold, and the documents in the population."""
    out, docs = {}, set()
    if system == "ward":
        import ladder_ward as W  # noqa: PLC0415
        g = W.S.load_gold(W.S.WARD / "gold")
        paths = {e["path"]: [W.doc_id(x) for x in e.get("message_ids") or []] for e in json.loads((W.S.WARD / "manifest.json").read_text())}
        rows = json.load(open(ROOT / "runs/quality-ledger-20261012/wt.json"))
        docs = {d for r in rows for f in r["latest"] for d in paths.get(f, []) if d in lines}
        for kind, items, val in (("stage_update", g["stage_updates"], "stage"), ("commitment", g["commitments"], None)):
            for s in items:
                for d in paths.get(s["file"], []):
                    if d in docs and (n := find(lines[d], s["quote"])) is not None:
                        out.setdefault((d, n), (kind, s.get(val) if val else None))
    elif system == "uv":
        import ladder_uv as LU  # noqa: PLC0415
        U = LU.U
        id_url = {str(d["id"]): d["url"] for d in map(json.loads, filter(str.strip, (U.ROOT / "raw/documents.jsonl").read_text().splitlines()))}
        g = json.loads((U.ROOT / "gold/cases.json").read_text())
        tune = {c["id"] for c in g["cases"] if c["fold"] == "tune"}
        for s in g["case_states"]:
            d = id_url.get(str(s["document"]))
            if s["case"] in tune and d in lines:
                docs.add(d)
                if (n := find(lines[d], s["quote"])) is not None:
                    out.setdefault((d, n), ("case_state", s["state"]))
    else:
        import ladder_gvc as G  # noqa: PLC0415
        mentions, bodies, keys = G.load_gold(G.GOLD, G.CORPUS)
        inv = {v: k for k, v in keys.items()}
        words = collections.defaultdict(list)
        for m in mentions.values():
            d = inv.get(m["doc"], m["doc"])
            if d not in lines:
                continue
            docs.add(d)
            b = bodies[m["doc"]]
            ls, le = b.rfind("\n", 0, m["start"]) + 1, b.find("\n", m["end"])
            n = find(lines[d], b[ls:le if le >= 0 else len(b)])
            if n is not None:
                out.setdefault((d, n), ("report", None))
                words[(d, n)].append(b[m["start"]:m["end"]].lower())
        out = {k: (v[0], "reference-only" if all(w in REFERENCE for w in words[k]) else None) for k, v in out.items()}
    return out, docs


def join():
    rng = random.Random(SEED)
    pop = {}
    for system, (leg, corpus) in SYSTEMS.items():
        rec = recipe(leg, corpus)
        kinds, subjects = plan(rec)
        lines, chose = trace(leg)
        docs_text = documents(leg, corpus)
        gold, docs = gold_lines(system, lines)
        pos, unasked = [], 0
        for (d, n), (kind, value) in sorted(gold.items()):
            ln = lines[d][n]
            if not ln["asked"]:
                unasked += 1
                continue
            cls = "hit" if ln["kind"] == kind else "lost"
            read = [c[3] for c in chose.get(d, []) if c[0] <= n <= c[1] and c[2] in [a["name"] for k in kinds for a in k["fields"]]]
            pos.append({"doc": d, "line": n, "gold": kind, "value": None if value == "reference-only" else value,
                        "reference_only": value == "reference-only", "class": cls, "logged": ln["dist"],
                        "logged_kind": ln["kind"], "choose": read[0] if read else None})
        if system == "gvc":
            lost = [p for p in pos if p["class"] == "lost"]
            hit = [p for p in pos if p["class"] == "hit"]
            pos = rng.sample(lost, min(GVC_CAP["lost"], len(lost))) + rng.sample(hit, min(GVC_CAP["hit"], len(hit)))
        marked = set(gold)
        pool = [(d, n) for d in sorted(docs) for n, ln in sorted(lines[d].items()) if ln["asked"] and (d, n) not in marked]
        neg = [{"doc": d, "line": n, "gold": None, "value": None, "reference_only": False,
                "class": "clean" if lines[d][n]["kind"] == "none" else "flagged", "logged": lines[d][n]["dist"],
                "logged_kind": lines[d][n]["kind"], "choose": None}
               for d, n in rng.sample(pool, min(len(pos), len(pool)))]
        items = sorted(pos + neg, key=lambda x: (x["doc"], x["line"]))
        views = {d: (facts(rec, docs_text[d][0]), numbered(docs_text[d][1])) for d in {x["doc"] for x in items}}
        for x in items:
            x["text"] = line_text(lines[x["doc"]][x["line"]]["text"], docs_text[x["doc"]][1][x["line"] - 1])
            x["unit"] = unit(docs_text[x["doc"]][1], x["line"])
        pop[system] = {"kinds": kinds, "subjects": subjects, "items": items, "positives_not_asked": unasked,
                       "documents": len(views), "views": {d: f + v for d, (f, v) in views.items()}}
        c = collections.Counter(x["class"] for x in items)
        print(f"{system}: documents {len(views)} items {len(items)} {dict(c)} positives not asked {unasked} "
              f"reference-only {sum(x['reference_only'] for x in items)}")
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "join.json").write_text(json.dumps(pop, indent=1))
    DATA.mkdir(exist_ok=True)
    summary = {s: {"documents": p["documents"], "positives_not_asked": p["positives_not_asked"],
                   "classes": dict(collections.Counter(x["class"] for x in p["items"])),
                   "reference_only": sum(x["reference_only"] for x in p["items"]),
                   "items": [[x["doc"], x["line"], x["gold"], x["value"], x["class"], x["unit"]] for x in p["items"]]}
               for s, p in pop.items()}
    (DATA / "population.json").write_text(json.dumps(summary, indent=1))
    return pop


# ---------------------------------------------------------------- calls (cached by their whole request)
def post(req):
    path = OUT / "calls" / (hashlib.sha256(json.dumps(req, sort_keys=True).encode()).hexdigest() + ".json")
    if path.exists():
        return json.loads(path.read_text())
    r = urllib.request.Request(CHAT, json.dumps(req).encode(), {"Content-Type": "application/json"})
    t0, rec = time.time(), {}
    for attempt in range(20):  # the daemon sheds past its queue budget with 503 + Retry-After: honour it
        try:
            with urllib.request.urlopen(r, timeout=900) as resp:
                rec["response"] = json.loads(resp.read())
            break
        except urllib.error.HTTPError as e:
            if e.code != 503 or attempt == 19:
                rec["error"] = f"HTTP {e.code}: {e.read()[:200]!r}"
                break
            time.sleep(float(e.headers.get("Retry-After") or 15))
        except (urllib.error.URLError, TimeoutError) as e:
            rec["error"] = f"{type(e).__name__}: {e}"
            break
    rec["wall_s"] = round(time.time() - t0, 2)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(rec))
    return rec


def answer(rec, labels):
    content = ((((rec or {}).get("response") or {}).get("choices") or [{}])[0].get("message") or {}).get("content") or ""
    try:
        d = json.loads(content)
    except json.JSONDecodeError:
        return None
    return d if isinstance(d, dict) and all(l in d for l in labels) else None


def questions(system, pop, arm, x):
    """[(key, system prompt, user, labels, meaning)] for one item under one arm."""
    view, kinds, subjects = pop["views"][x["doc"]], pop["kinds"], pop["subjects"]
    if arm == "C":
        return [("C",) + q_control(view, x["line"], x["text"], kinds)]
    if arm == "V":
        return [("V",) + q_values(view, x["line"], x["text"], kinds)] if any(k["fields"] for k in kinds) else []
    if arm == "Y":
        return [(f"Y:{k['name']}",) + q_yes(view, x["line"], x["text"], k) + ({"A": (k["name"], None)},) for k in kinds]
    if arm == "T":
        return [(f"T:{s['name']}",) + q_about(view, x["line"], x["text"], s) + ({"A": (s["name"], None)},) for s in subjects]
    raise SystemExit(f"unknown arm {arm}")


def run(arms):
    pop = json.loads((OUT / "join.json").read_text())
    for arm in arms:
        for system, p in pop.items():
            t0, n, failed = time.time(), 0, 0
            for x in p["items"]:
                for _, sysp, user, labels, _ in questions(system, p, arm, x):
                    rec = post(body(sysp, user, labels))
                    n += 1
                    failed += answer(rec, labels) is None
            print(f"arm {arm} {system}: {n} questions, {failed} without a distribution, {time.time() - t0:.0f}s", flush=True)


# ---------------------------------------------------------------- scoring (cache only)
def outcome(system, p, arm, x):
    """(p_right, predicted kinds, predicted value) from the cache, or None when a question is unanswered."""
    qs = questions(system, p, arm, x)
    if not qs:
        return None
    got = []
    for key, sysp, user, labels, meaning in qs:
        path = OUT / "calls" / (hashlib.sha256(json.dumps(body(sysp, user, labels), sort_keys=True).encode()).hexdigest() + ".json")
        if not path.exists():
            return None
        d = answer(json.loads(path.read_text()), labels)
        if d is None:
            return None
        got.append((key, d, meaning))
    if arm in ("C", "V"):
        _, d, meaning = got[0]
        mass = collections.Counter()
        for l, pr in d.items():
            mass[meaning[l][0] if l in meaning else None] += pr
        best = max(d, key=d.get)
        kind = meaning[best][0] if best in meaning else None
        value = meaning[best][1] if best in meaning else None
        p_right = mass[x["gold"]] if x["gold"] else mass[None]
        return p_right, ({kind} if kind else set()), value, d
    if arm == "Y":
        yes = {meaning["A"][0]: d["A"] / max(d["A"] + d[NONE], 1e-9) for _, d, meaning in got}
        kinds = {k for k, py in yes.items() if py > 0.5}
        p_right = math.prod(py if (k == x["gold"]) else 1 - py for k, py in yes.items())
        return p_right, kinds, None, yes
    if arm == "T":
        _, d, meaning = got[0]
        py = d["A"] / max(d["A"] + d[NONE], 1e-9)
        return (py if x["gold"] else 1 - py), ({meaning["A"][0]} if py > 0.5 else set()), None, {"yes": py}


def score():
    pop = json.loads((OUT / "join.json").read_text())
    report, rows = {}, []
    for system, p in pop.items():
        items, res = p["items"], {}
        # the gate: C re-asked against the logged distribution
        agree = dp = judged = 0
        for x in items:
            o = outcome(system, p, "C", x)
            if o is None:
                continue
            judged += 1
            logged_best = max(x["logged"], key=x["logged"].get)
            agree += max(o[3], key=o[3].get) == logged_best
            dp += abs(o[3].get(NONE, 0) - x["logged"].get(NONE, 0))
        gate = {"judged": judged, "of": len(items), "argmax_agree": round(agree / judged, 3) if judged else None,
                "mean_abs_dp_none": round(dp / judged, 4) if judged else None}
        gate["passed"] = bool(judged == len(items) and gate["argmax_agree"] >= 0.95 and gate["mean_abs_dp_none"] <= 0.05)
        for arm in ("C", "V", "Y", "T"):
            outs = [(x, outcome(system, p, arm, x)) for x in items]
            if all(o is None for _, o in outs):
                continue
            if any(o is None for _, o in outs):
                res[arm] = {"could_not_judge": f"{sum(o is None for _, o in outs)} items unanswered"}
                continue
            pos = [(x, o) for x, o in outs if x["gold"]]
            neg = [(x, o) for x, o in outs if not x["gold"]]
            right = lambda x, o: (x["gold"] in o[1]) if x["gold"] else not o[1]  # noqa: E731
            lost = [(x, o) for x, o in pos if x["class"] == "lost" and not x["reference_only"]]
            hits = [(x, o) for x, o in pos if x["class"] == "hit"]
            valued = [(x, o) for x, o in pos if x["value"] and x["gold"] in o[1] and o[2] is not None]
            chose = [(x, o) for x, o in valued if x["choose"] is not None]
            r = {"mean_log_p_right": round(sum(math.log(max(o[0], 1e-6)) for _, o in outs) / len(outs), 4),
                 "lost_recovered": [sum(right(x, o) for x, o in lost), len(lost)],
                 "hits_kept": [sum(right(x, o) for x, o in hits), len(hits)],
                 "unmarked_flagged": [sum(not right(x, o) for x, o in neg), len(neg)],
                 "reference_only_right": [sum(right(x, o) for x, o in pos if x["reference_only"]), sum(x["reference_only"] for x, _ in pos)]}
            if valued:
                r["value_right"] = [sum(o[2] == x["value"] for x, o in valued), len(valued)]
                r["choose_logged_right_same_lines"] = [sum(x["choose"] == x["value"] for x, _ in chose), len(chose)]
            r["lost_recovered_by_unit"] = {u: [sum(right(x, o) for x, o in lost if x["unit"] == u), sum(x["unit"] == u for x, _ in lost)]
                                           for u in sorted({x["unit"] for x, _ in lost})}
            res[arm] = r
            for x, o in outs:
                rows.append({"system": system, "arm": arm, "doc": x["doc"], "line": x["line"], "gold": x["gold"],
                             "value": x["value"], "class": x["class"], "unit": x["unit"], "p_right": round(o[0], 4),
                             "predicted": sorted(o[1]), "predicted_value": o[2], "text": x["text"][:160]})
        report[system] = {"gate": gate, "arms": res}
        print(f"== {system} gate {json.dumps(gate)}")
        for arm, r in res.items():
            print(f"   {arm} {json.dumps(r)}")
    (DATA / "score.json").write_text(json.dumps(report, indent=1))
    with open(DATA / "rows.jsonl", "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "join"
    if cmd == "join":
        join()
    elif cmd == "run":
        run(sys.argv[2:] or ["C"])
    elif cmd == "score":
        score()
    else:
        raise SystemExit(__doc__)
