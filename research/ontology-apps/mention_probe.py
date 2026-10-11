"""Can the served model point at each happening in a located GVC line? An offline probe for a Mention pass
(ONTOLOGY_METHOD §Reading; order ontology-layer-13-gvc-mention-probe).

    python3 mention_probe.py sample   # writes mention-probe/sample.json, no model call
    python3 mention_probe.py run      # asks each sampled line once, cached under runs/mention-probe/calls/
    python3 mention_probe.py score    # mention-probe/score.json and lines.jsonl, from the cache only

LINES: the recorded half of runs/e4-locate-values/gvc's Locate trace (ladder.recorded_half + ladder.LOCATED), each
located line (`report`) mapped to the gold body by its logged text: the reader's line n is matched, in order, to the
next non-empty body line equal to it (or starting with it when the log's 160-character excerpt cut it). A located
line that matches no body line (the title the extractor prepends, which gold's offsets do not cover) is counted and
left out. Gold: ladder_gvc.load_gold (gold.json under the baseline's gvc/statements); a line holds a mention when the
mention's characters lie inside it.

SAMPLE RULE (committed before any call): from the mapped located lines, MULTI = those holding 2 or more gold
mentions, SINGLE = those holding exactly 1; random.Random(13).sample(sorted by (doc, line)) of 100 from each.

THE QUESTION: the recipe's subject type (cdcr/recipe-gvc-e2e.toml: name, description; no other domain word), the
document's title, and the line; it asks for every span of the line that names one particular of the type, verbatim.
Reader's conditions (passes.rs locate_question, inference_client/wire.rs): temperature 0, thinking off (think_budget
0, chat_template_kwargs.enable_thinking false), response_format json_schema strict. One call per line, no retry.

VERIFICATION: each answered span is placed in the line in code, in answer order, at its first occurrence not already
taken (exact text). A span not in the line is refused, counted, never kept. A call that fails or answers no parseable
JSON leaves the line with no spans and is counted.

SCORING: a verified span overlaps a gold mention when they share a character.
  recall     = gold mentions (both strata) overlapped by some verified span / gold mentions
  precision  = verified spans that overlap at least one gold mention, all of one chain / verified spans
  split      = MULTI lines where every gold chain in the line is overlapped by some verified span and no verified span
               overlaps mentions of two chains / MULTI lines
  calls/line = calls / lines

WORTH A PASS (pre-registered, committed before scoring): recall >= .70 AND precision >= .70 AND split >= .50 AND
calls/line <= 2. Any one below is "not worth a pass" on this question. If more than 10% of calls fail (no answer), the
verdict is could-not-judge, not a fail.
"""
import collections, hashlib, json, pathlib, random, sys, time, tomllib, urllib.error, urllib.request

import ladder as L
import ladder_gvc as G

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
RUN = ROOT / "runs/e4-locate-values/gvc"
OUT = ROOT / "runs/mention-probe"  # the call cache (gitignored, as runs/ is)
DATA = HERE / "mention-probe"  # the committed sample, table and per-line spans
RECIPE = HERE / "cdcr/recipe-gvc-e2e.toml"
SEED, PER_STRATUM = 13, 100
URL, MODEL = "http://localhost:9741/v1/chat/completions", "commonwealth/primary"
BARS = {"recall": 0.70, "precision": 0.70, "split": 0.50, "calls_per_line": 2.0}
MAX_FAILED = 0.10

SYSTEM = ("You read one line of a document and point at each place in it that names a particular {name}.\n\n"
          "The user message gives the declared type with what it means, the document's title, and the line. Answer "
          "with every span of the line that names one particular {name}, each copied exactly as it is written in the "
          "line, in the order they appear. When two spans name the same {name}, list both. Answer with an empty list "
          "if the line names none.")
SCHEMA = {"type": "object", "additionalProperties": False, "required": ["spans"],
          "properties": {"spans": {"type": "array", "maxItems": 16, "items": {"type": "string"}}}}


def subject_type():
    """The recipe's one subject type: the type its one claim kind is about."""
    types = tomllib.loads(RECIPE.read_text())["enrichment"]["ontology"]["types"]
    subjects = {t["subject"] for t in types if t.get("kind") == "claim"}
    if len(subjects) != 1:
        sys.exit(f"{RECIPE} declares {len(subjects)} claim subjects; the probe asks about exactly one")
    return next(t for t in types if t["name"] in subjects)


def located_lines():
    """[{doc, n, start, end, text, mentions: [mention ids]}] for each located line mapped to the gold body, and counts."""
    mentions, bodies, keys = G.load_gold(G.GOLD, G.CORPUS)
    debug, stop = L.recorded_half(RUN / "job.debug.log")
    if stop is None:
        sys.exit(f"{RUN}/job.debug.log has no replay marker: cannot tell the recorded half")
    per = collections.defaultdict(list)
    for raw in debug:
        m = L.LOCATED.search(raw)
        if m:
            per[m.group(1)].append((int(m.group(2)), m.group(3), raw[m.end():].rstrip("\n")))
    by_doc = collections.defaultdict(list)
    for mid, m in mentions.items():
        by_doc[m["doc"]].append(mid)
    counts, out = collections.Counter(), []
    for key, seen in per.items():
        doc = keys.get(key)
        if doc is None:
            counts["documents_not_in_gold"] += 1
            continue
        body, nonempty, off = bodies[doc], [], 0
        for raw in body.split("\n"):
            t = raw.strip()
            if t:
                s = off + len(raw) - len(raw.lstrip())
                nonempty.append((s, s + len(t), t))
            off += len(raw) + 1
        j = 0
        for n, kind, text in sorted(seen):
            hit = next((i for i in range(j, len(nonempty)) if nonempty[i][2] == text
                        or (len(text) >= 150 and nonempty[i][2].startswith(text))), None)
            if hit is None:
                counts[f"located_{kind}_not_in_body"] += 1
                continue
            j = hit + 1
            if kind != "report":  # the recipe's one claim kind; "none" lines are not located
                continue
            s, e, t = nonempty[hit]
            ms = sorted(mid for mid in by_doc[doc] if s <= mentions[mid]["start"] and mentions[mid]["end"] <= e)
            out.append({"doc": doc, "n": n, "start": s, "end": e, "text": t, "mentions": ms})
            counts["located_report_mapped"] += 1
    return out, mentions, dict(counts)


def sample():
    lines, _, counts = located_lines()
    lines.sort(key=lambda x: (x["doc"], x["n"]))
    multi = [x for x in lines if len(x["mentions"]) >= 2]
    single = [x for x in lines if len(x["mentions"]) == 1]
    rng = random.Random(SEED)
    picked = {"multi": rng.sample(multi, min(PER_STRATUM, len(multi))),
              "single": rng.sample(single, min(PER_STRATUM, len(single)))}
    DATA.mkdir(parents=True, exist_ok=True)
    rec = {"rule": f"random.Random({SEED}).sample of {PER_STRATUM} per stratum, lines sorted by (doc, n)",
           "population": {"multi": len(multi), "single": len(single), "zero": sum(not x["mentions"] for x in lines),
                          **counts}, "lines": picked}
    (DATA / "sample.json").write_text(json.dumps(rec, indent=1))
    print(json.dumps(rec["population"]))
    return rec


def titles():
    return {d["id"]: d["title"] for d in G.jsonl(G.CORPUS / "raw/documents.jsonl")}


def question(t, title, line):
    user = (f"Type: {t['name']} ({t['description']})\n\nDocument title: \"{title}\"\n\nLine: \"{line}\"\n\n"
            f"Which spans of the line each name one particular {t['name']}?")
    return SYSTEM.format(name=t["name"]), user


def request(system, user):
    """The request body and its cache path: the key is the whole input."""
    body = {"model": MODEL, "temperature": 0, "max_tokens": 512, "think_budget": 0, "thinking": {"type": "disabled"},
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "response_format": {"type": "json_schema", "json_schema": {"name": "mentions", "strict": True, "schema": SCHEMA}}}
    return body, OUT / "calls" / f"{hashlib.sha1(json.dumps(body, sort_keys=True).encode()).hexdigest()[:16]}.json"


def ask(system, user):
    """One call, cached by its whole input; ({"answer"| "error", usage, ...}, cached)."""
    body, path = request(system, user)
    if path.exists():
        return json.loads(path.read_text()), True
    req = urllib.request.Request(URL, json.dumps(body).encode(), {"Content-Type": "application/json"})
    t0, rec = time.time(), {"request": body}
    for attempt in range(20):  # the daemon sheds past its queue budget with 503 + Retry-After: honour it
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                resp = json.loads(r.read())
            break
        except urllib.error.HTTPError as e:
            if e.code != 503 or attempt == 19:
                rec["error"] = f"HTTP {e.code}"
                resp = None
                break
            time.sleep(float(e.headers.get("Retry-After") or 15))
    rec.update(wall_s=round(time.time() - t0, 2), shed_retries=attempt)
    if resp is not None:
        rec["response"] = resp
        content = resp["choices"][0]["message"].get("content") or ""
        try:
            rec["answer"] = json.loads(content)
        except json.JSONDecodeError:
            rec["error"] = f"not JSON: {content[:120]!r}"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(rec, indent=1))
    return rec, False


def verify(spans, line):
    """[(start, end)] in line coordinates for spans found in the line, and the refused spans."""
    taken, kept, refused = [], [], []
    for sp in spans:
        if not isinstance(sp, str) or not sp.strip():
            refused.append(sp)
            continue
        at, place = 0, None
        while (i := line.find(sp, at)) >= 0:
            if all(i + len(sp) <= a or b <= i for a, b in taken):
                place = (i, i + len(sp))
                break
            at = i + 1
        if place is None:
            refused.append(sp)
        else:
            taken.append(place)
            kept.append(place)
    return kept, refused


def probe(call=True):
    s = json.loads((DATA / "sample.json").read_text())
    t, tt = subject_type(), titles()
    rows = []
    for stratum, lines in s["lines"].items():
        for x in lines:
            system, user = question(t, tt[x["doc"]], x["text"])
            if call:
                rec, _ = ask(system, user)
            else:  # score reads the cache only: a missing record is never-ran for that line
                path = request(system, user)[1]
                rec = json.loads(path.read_text()) if path.exists() else None
            rows.append({"stratum": stratum, **x, "record": rec})
    return rows


def score():
    rows = probe(call=False)
    _, mentions, _ = located_lines()
    tally = collections.Counter()
    per_line = []
    for r in rows:
        rec = r["record"]
        tally["lines"] += 1
        if rec is None:
            tally["never_ran"] += 1
            continue
        tally["calls"] += 1
        u = (rec.get("response") or {}).get("usage") or {}
        tally["prompt_tokens"] += u.get("prompt_tokens", 0)
        tally["completion_tokens"] += u.get("completion_tokens", 0)
        if "answer" not in rec:
            tally["failed_calls"] += 1
            spans, refused = [], []
        else:
            spans, refused = verify(rec["answer"].get("spans", []), r["text"])
        tally["answered_spans"] += len(spans) + len(refused)
        tally["refused_spans"] += len(refused)
        tally["verified_spans"] += len(spans)
        gold = [(mentions[m]["start"] - r["start"], mentions[m]["end"] - r["start"], mentions[m]["chain"])
                for m in r["mentions"]]
        hit = [any(a < ge and gs < b for a, b in spans) for gs, ge, _ in gold]
        tally["gold"] += len(gold)
        tally["gold_hit"] += sum(hit)
        tally[f"gold_{r['stratum']}"] += len(gold)
        tally[f"gold_hit_{r['stratum']}"] += sum(hit)
        mixed = 0
        for a, b in spans:
            chains = {c for gs, ge, c in gold if a < ge and gs < b}
            tally["span_tp"] += len(chains) == 1
            tally["span_no_gold"] += not chains
            mixed += len(chains) > 1
        tally["span_mixed"] += mixed
        if r["stratum"] == "multi":
            chains = {c for *_, c in gold}
            covered = {c for (gs, ge, c), h in zip(gold, hit) if h}
            ok = covered == chains and not mixed
            tally["multi_lines"] += 1
            tally["multi_split_ok"] += ok
            tally["multi_one_chain"] += len(chains) == 1
        per_line.append({"stratum": r["stratum"], "doc": r["doc"], "n": r["n"], "line": r["text"],
                         "gold": [r["text"][gs:ge] for gs, ge, _ in gold],
                         "spans": [r["text"][a:b] for a, b in spans], "refused": refused})
    lines = tally["lines"] - tally["never_ran"]
    f = lambda a, b: round(a / b, 3) if b else None  # noqa: E731
    table = {"lines": tally["lines"], "never_ran": tally["never_ran"], "failed_calls": tally["failed_calls"],
             "recall": f(tally["gold_hit"], tally["gold"]),
             "recall_multi": f(tally["gold_hit_multi"], tally["gold_multi"]),
             "recall_single": f(tally["gold_hit_single"], tally["gold_single"]),
             "precision": f(tally["span_tp"], tally["verified_spans"]),
             "spans_no_gold": tally["span_no_gold"], "spans_two_chains": tally["span_mixed"],
             "split": f(tally["multi_split_ok"], tally["multi_lines"]),
             "multi_lines_one_chain": tally["multi_one_chain"],
             "refused_spans": tally["refused_spans"], "answered_spans": tally["answered_spans"],
             "calls_per_line": f(tally["calls"], lines),
             "prompt_tokens_per_line": f(tally["prompt_tokens"], lines),
             "completion_tokens_per_line": f(tally["completion_tokens"], lines)}
    if tally["never_ran"]:
        verdict = "never-ran (lines without a cached answer)"
    elif tally["failed_calls"] > MAX_FAILED * tally["calls"]:
        verdict = "could-not-judge (more than 10% of calls failed)"
    else:
        misses = [k for k in ("recall", "precision", "split") if (table[k] or 0) < BARS[k]]
        if table["calls_per_line"] > BARS["calls_per_line"]:
            misses.append("calls_per_line")
        verdict = "worth a pass" if not misses else f"not worth a pass (below: {', '.join(misses)})"
    out = {"bars": BARS, "table": table, "verdict": verdict}
    (DATA / "score.json").write_text(json.dumps(out, indent=1))
    with open(DATA / "lines.jsonl", "w") as fh:
        for x in per_line:
            fh.write(json.dumps(x) + "\n")
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    {"sample": sample, "run": lambda: probe(call=True), "score": score}[sys.argv[1]]()
