#!/usr/bin/env python3
"""K2 query-layer probe (feature-fidelity campaign, Ontology: "Query-layer probe", pre-registered
2026-10-03): can the local primary model turn a question into a typed query over the DECLARED
ontology, so that code, not the model, executes the semantics?

    python3 k2_query.py [--runs 2] [--json k2-query.json]

GRAMMAR, closed, built from the fixture's declared ontology (atlas/ontology.json, checked equal to
recipe-dev-b.toml's types), never from K2:
    {target_type: an entity type,
     filters:   [{attribute: `name` or one of the target's own non-ref attributes, op: eq|lt|gt|contains,
                  value, negate}],
     relations: [{relation: a declared relation or ref, other_type: its far end, other_name, negate,
                  where: null | {filters: the far end's own, relations: the far end's, named, no where}}],
     aggregate: none|count|argmax|argmin, aggregate_over: a target attribute or a related type | null}
Iteration 2 (campaign Decisions 2026-10-03, amended before running) added `where`, filter `negate` and
`name`: iteration 1 could not express a two-hop join or a filter on the related type. Iteration 3 added
one example of a NAMED relation inside `where`: iteration 2's model never put a name there.
One JSON Schema `anyOf` branch per target type, so each branch's enums hold only what that type
declares. It is sent as `response_format: {type: json_schema}`: the serving host lifts the schema
into `structured_output` (sovereign-serving-host/src/inference_adapter.rs:415-416, via
`extract_response_format_schema` :1000) and the sampler compiles it with llguidance, refusing the
request if it does not compile (sovereign-inference/src/embedded/sampler.rs:342-358). Thinking off:
`chat_template_kwargs.enable_thinking=false` (inference_adapter.rs:544) and `think_budget: 0`
(:379, `resolve_think_budget` :976). Every output is also validated here with `jsonschema`; an
invalid one is retried once and counted.

PROMPT: the declared ontology rendered as documentation, the grammar, two examples from an
unrelated domain (a library), and ONE question. No K2 question or paraphrase.

EXECUTOR: runs a query over the same `Records` primitives k2_t0.py's hand-written programs use
(`has`, `in_region`, `burial`, `members`, `labels`, `resolve`, `_argmax`), so PARSE agreement
(executed answer == k2_t0 program answer, exact set / int / single) compares like with like.
END-TO-END is k2_t0's own `score_records` against gold. Two instrument checks run before any
model output is trusted: a REFERENCE query per question (built from K2's params, never shown to
the model) must reproduce the program's answer, and the program run over GOLD-COMPLETE records
(the bank's attested facts, k2_bank.Facts) must reproduce gold. Agreement is also reported on those
gold-complete records, because on the sparse atlas many program answers are empty or 0 and any
query about the same mint agrees with them (counted as `vacuous`).
"""
import argparse, collections, copy, hashlib, json, pathlib, re, sys, time, tomllib, urllib.error, urllib.request

import jsonschema

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from make_bank import fold, rx  # noqa: E402
from k2_t0 import IDX, LISTS, Records, score_records  # noqa: E402  (the T0 programs and scorer)
import k2_bank  # noqa: E402  (the bank's facts, for gold-complete records)

CHAT = "http://127.0.0.1:9741/v1/chat/completions"
MODEL = "primary"
CORPUS = "ft-ans-dev-b"
ATLAS = IDX / CORPUS / "atlas"
RECIPE = HERE / "recipe-dev-b.toml"
OPS = ["eq", "lt", "gt", "contains"]
AGGS = ["none", "count", "argmax", "argmin"]
# The declared ontology leaves every attribute description empty; these glosses are written
# from the attribute's name, type and the ontology's own prose guidance, nothing else.
GLOSS = {
    ("hoard", "findspot"): "the place where the hoard was found (a place name, region or country)",
    ("hoard", "found"): "when the hoard was discovered (a year)",
    ("hoard", "buried"): "when the hoard was buried (a year, or a span of years)",
    ("coin", "denomination"): "the coin's denomination (stater, tetradrachm, drachm ...)",
    ("coin", "metal"): "the coin's metal",
    ("coin", "weight"): "the coin's weight",
    ("coin", "struck"): "when the coin was struck (a year, or a span of years)",
}
REL_GLOSS = {"ref": "{from_} has a `{attr}` reference to one {to}"}


# ── the grammar, from the declared ontology ────────────────────────────────
def declared():
    onto = json.loads((ATLAS / "ontology.json").read_text())["policies"]
    types = onto["shape"]["types"]
    recipe = tomllib.loads(RECIPE.read_text())["enrichment"]["ontology"]["types"]
    names = lambda ts: sorted((t["name"], t["kind"]) for t in ts)  # noqa: E731
    if names(types) != names(recipe):
        sys.exit(f"REFUSED: atlas ontology.json types {names(types)} != recipe {names(recipe)}")
    return types, onto["prose"]["guidance"]


def grammar(types):
    """-> (entity types, per-type attributes, per-type edges [(relation, other_type, direction gloss)])."""
    ents = [t["name"] for t in types if t["kind"] == "entity"]
    attrs = {t["name"]: [a for a in t.get("attributes") or [] if a["type"] != "ref"] for t in types if t["kind"] == "entity"}
    edges = collections.defaultdict(list)
    for t in types:
        if t["kind"] == "relation" and t.get("from") in ents and t.get("to") in ents:
            edges[t["from"]].append((t["name"], t["to"], f"this {t['from']} {t['name']} that {t['to']}"))
            edges[t["to"]].append((t["name"], t["from"], f"that {t['from']} {t['name']} this {t['to']}"))
        for a in t.get("attributes") or []:
            if a["type"] == "ref" and t["name"] in ents and a.get("of") in ents:
                r = f"{t['name']}.{a['name']}"
                edges[t["name"]].append((r, a["of"], f"this {t['name']}'s {a['name']} is that {a['of']}"))
                edges[a["of"]].append((r, t["name"], f"that {t['name']}'s {a['name']} is this {a['of']}"))
    return ents, attrs, dict(edges)


def schema(ents, attrs, edges):
    def filters(t):
        return {"type": "array", "maxItems": 4, "items": {
            "type": "object", "additionalProperties": False, "required": ["attribute", "op", "value", "negate"],
            "properties": {"attribute": {"enum": ["name"] + [a["name"] for a in attrs[t]]}, "op": {"enum": OPS},
                           "value": {"type": ["string", "number"]}, "negate": {"type": "boolean"}}}}

    def relations(t, depth):
        items = []
        for r, o, _ in edges.get(t, []):
            props = {"relation": {"const": r}, "other_type": {"const": o},
                     "other_name": {"type": ["string", "null"]}, "negate": {"type": "boolean"}}
            if depth == 1:                                        # the far end's own conditions; depth 2 ends here
                props["where"] = {"anyOf": [{"type": "null"}, {
                    "type": "object", "additionalProperties": False, "required": ["filters", "relations"],
                    "properties": {"filters": filters(o), "relations": relations(o, 2)}}]}
            items.append({"type": "object", "additionalProperties": False, "required": list(props), "properties": props})
        return {"type": "array", "maxItems": 4, "items": {"anyOf": items}} if items else {"type": "array", "maxItems": 0}

    branches = []
    for t in ents:
        over = [a["name"] for a in attrs[t] if a["type"] in ("time", "quantity")] + sorted({o for _, o, _ in edges.get(t, [])})
        props = {
            "target_type": {"const": t},
            "filters": filters(t),
            "relations": relations(t, 1),
            "aggregate": {"enum": AGGS},
            "aggregate_over": {"enum": [None] + over},
        }
        branches.append({"type": "object", "additionalProperties": False, "properties": props,
                         "required": list(props)})
    return {"anyOf": branches}


def documentation(types, guidance, ents, attrs, edges):
    L = ["KNOWLEDGE BASE SCHEMA", "", guidance.strip(), "", "Entity types and their attributes:"]
    for t in [t for t in types if t["kind"] == "entity"]:
        L.append(f"- {t['name']}: {t.get('description') or ''}".rstrip())
        for a in attrs[t["name"]]:
            kind = a["type"] + (" range" if a.get("range") else "") + (f", unit {a['unit']}" if a.get("unit") else "") \
                + (f", one of {a['values']}" if a.get("values") else "")
            L.append(f"    {a['name']} ({kind}): {GLOSS.get((t['name'], a['name']), '')}")
    L += ["", "Relations (each usable from either end; `other_type` names the far end):"]
    seen = set()
    for t in ents:
        for r, o, gloss in edges.get(t, []):
            if (t, r, o) not in seen:
                seen.add((t, r, o))
                L.append(f"- from a {t}: relation `{r}`, other_type `{o}` — {gloss}")
    rel_desc = {t["name"]: t["description"] for t in types if t["kind"] == "relation"}
    for r, d in rel_desc.items():
        L.append(f"  `{r}` means: {d}")
    L += ["", "QUERY GRAMMAR (answer with exactly one JSON object):",
          "- target_type: the kind of thing the question asks for.",
          "- filters: conditions on the target's OWN attributes, or on `name` (the entity's own name).",
          "  op `eq` = equals, `contains` = the attribute's text mentions the value, `lt` / `gt` =",
          "  earlier/smaller or later/larger than the value. negate=true keeps the targets that do NOT",
          "  meet the condition. Times are years as signed numbers: years B.C. are negative",
          "  (318 B.C. = -318), A.D. positive.",
          "- relations: the target must (negate=false) or must not (negate=true) be linked by `relation`",
          "  to an entity of `other_type` named `other_name` (null = to any such entity) that also meets",
          "  `where` (null = no further condition). `where` holds that linked entity's own filters and",
          "  its own relations to named entities, so a relation can reach entities that are described",
          "  rather than named.",
          "- aggregate: none = list every target that matches; count = how many targets match;",
          "  argmax / argmin = the matching target with the largest / smallest `aggregate_over`, which is",
          "  one of the target's time or quantity attributes, or a related type (= how many distinct",
          "  linked entities of that type the target has, counting only those that meet the `where` of",
          "  the query's relation to that type).", "",
          "EXAMPLES (a different knowledge base: books, authors, libraries):",
          'Q: Which books printed before 1600 are held by the Bodleian?',
          'A: {"target_type": "book", "filters": [{"attribute": "printed", "op": "lt", "value": 1600, "negate": false}], '
          '"relations": [{"relation": "held_by", "other_type": "library", "other_name": "Bodleian", "negate": false, '
          '"where": null}], "aggregate": "none", "aggregate_over": null}',
          'Q: Which author has written the most books?',
          'A: {"target_type": "author", "filters": [], "relations": [], "aggregate": "argmax", "aggregate_over": "book"}',
          'Q: Which authors wrote a book printed before 1500?',
          'A: {"target_type": "author", "filters": [], "relations": [{"relation": "wrote", "other_type": "book", '
          '"other_name": null, "negate": false, "where": {"filters": [{"attribute": "printed", "op": "lt", '
          '"value": 1500, "negate": false}], "relations": []}}], "aggregate": "none", "aggregate_over": null}',
          'Q: Which libraries hold a book written by Austen?',
          'A: {"target_type": "library", "filters": [], "relations": [{"relation": "held_by", "other_type": "book", '
          '"other_name": null, "negate": false, "where": {"filters": [], "relations": [{"relation": "wrote", '
          '"other_type": "author", "other_name": "Austen", "negate": false}]}}], "aggregate": "none", "aggregate_over": null}']
    return "\n".join(L)


SYSTEM = ("You translate a question about a knowledge base into one typed query over its schema. "
          "Use only the types, attributes and relations the schema declares. Output only the JSON query.")


# ── the model ──────────────────────────────────────────────────────────────
def ask(doc, sch, question):
    body = {"model": MODEL, "temperature": 0, "max_tokens": 400, "think_budget": 0,
            "chat_template_kwargs": {"enable_thinking": False},
            "response_format": {"type": "json_schema", "json_schema": {"name": "typed_query", "schema": sch, "strict": True}},
            "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": f"{doc}\n\nQuestion: {question}"}]}
    for attempt in range(6):
        t0 = time.time()
        try:
            req = urllib.request.Request(CHAT, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            d = json.load(urllib.request.urlopen(req, timeout=300))
            return d["choices"][0]["message"].get("content") or "", round(time.time() - t0, 2)
        except urllib.error.HTTPError as e:
            if e.code != 503 or attempt == 5:
                raise RuntimeError(f"chat {e.code}: {e.read()[:400]!r}")
        except (urllib.error.URLError, TimeoutError, ConnectionError) as e:
            if attempt == 5:
                raise
            print(f"chat retry {attempt + 1}: {e}", file=sys.stderr)
        time.sleep(3 * (attempt + 1))


def parse(doc, sch, question):
    """-> (query or None, raw, seconds, attempts). One validation retry."""
    raw, secs = "", 0.0
    for attempt in (1, 2):
        raw, s = ask(doc, sch, question)
        secs += s
        try:
            q = json.loads(raw)
            jsonschema.validate(q, sch)
            return q, raw, secs, attempt
        except (json.JSONDecodeError, jsonschema.ValidationError):
            continue
    return None, raw, secs, 2


# ── the executor, over k2_t0's Records primitives ───────────────────────────
def year(v):
    """Signed year from a query value: -318, "318 B.C.", "c. 318 BC" -> -318; 1905 -> 1905."""
    if isinstance(v, (int, float)):
        return float(v)
    m = re.search(r"-?\d+(?:\.\d+)?", str(v))
    if not m:
        return None
    n = float(m.group(0))
    return -abs(n) if re.search(r"\bB\.?\s?C\.?E?\b", str(v), re.I) else n


def signed_interval(rec, g):
    b = rec.burial[g]
    return (-b[0], -b[1]) if b else None          # (earliest, latest) as signed years


def interval_of(text):
    ys = [int(y) for y in re.findall(r"(?<!\d)(\d{3,4})(?!\d)", text or "")]
    if not ys:
        return None
    bc = bool(re.search(r"\bB\.?\s?C", text or "", re.I))
    s = [-y if bc else y for y in ys]
    return (min(s), max(s))


def compare(iv, op, v):
    if iv is None or v is None:
        return False
    lo, hi = iv
    return hi < v if op == "lt" else lo > v if op == "gt" else lo <= v <= hi if op == "eq" else False


class Executor:
    def __init__(self, rec, ents=None):
        self.rec, self.ents = rec, ents or {}
        self.coins = {i: e for i, e in self.ents.items() if e.get("entity_type") == "coin"}
        self.rulers = {i: e for i, e in self.ents.items() if e.get("entity_type") == "ruler"}
        mint_names = [e["canonical_name"] for e in self.ents.values() if e.get("entity_type") == "mint"]
        self.mint_labels = sorted(set().union(*rec.members.values(), *[rec.labels(n) for n in mint_names]))

    def hoard_groups(self, name):
        if not name:
            return list(self.rec.groups)
        hid = self.rec.resolve(name)
        return [g for g in self.rec.groups if self.rec.resolved[g] == hid
                or fold(name) in fold(self.rec.display(g))]

    def group_ids(self, g):
        return {e["id"] for e in self.rec.groups[g]}

    def coin_named(self, c, name):
        return not name or bool(rx(name).search(fold(c.get("canonical_name") or "")))

    # one predicate per (target, relation, other_type), all built on Records primitives
    def related(self, target, x, r, other, name):
        rec = self.rec
        if r == "holds_coins_of":
            if target == "hoard":
                return bool(rec.members[x]) if not name else any(rec.has(x, m) for m in rec.labels(name))
            if target == "mint":
                return any(rec.has(g, x) for g in self.hoard_groups(name))
        if r == "coin.hoard":
            if target == "hoard":
                return any(c.get("attributes", {}).get("hoard") in self.group_ids(x) and self.coin_named(c, name)
                           for c in self.coins.values())
            if target == "coin":
                return any((self.coins[x].get("attributes") or {}).get("hoard") in self.group_ids(g) for g in self.hoard_groups(name))
        if r == "coin.mint":
            if target == "coin":
                return bool(rec.labels(rec._mint_of(self.coins[x]) or "") & (rec.labels(name) if name else set(self.mint_labels)))
            if target == "mint":
                return any(x in rec.labels(rec._mint_of(c) or "") and self.coin_named(c, name) for c in self.coins.values())
        if r == "coin.ruler":
            if target == "coin":
                v = (self.coins[x].get("attributes") or {}).get("ruler")
                rn = self.ents[v]["canonical_name"] if v in self.ents else v
                return bool(rn) and (not name or bool(rx(name).search(fold(rn))))
            if target == "ruler":
                return any((c.get("attributes") or {}).get("ruler") == x and self.coin_named(c, name) for c in self.coins.values())
        return False

    def attr(self, target, x, a):
        if target == "hoard":
            if a == "buried":
                return signed_interval(self.rec, x)
            if a == "findspot":
                return self.rec.findspot[x]
            return interval_of(" ".join((e.get("attributes") or {}).get(a) or "" for e in self.rec.groups[x]))
        if target == "coin":
            v = (self.coins[x].get("attributes") or {}).get(a)
            return interval_of(str(v)) if a in ("struck", "weight") and v is not None else v
        return None

    def is_named(self, t, x, name):
        if t == "hoard":
            return x in self.hoard_groups(str(name))
        if t == "mint":
            return x in self.rec.labels(str(name))
        return bool(rx(str(name)).search(fold((self.ents.get(x) or {}).get("canonical_name") or "")))

    def candidates(self, t):
        return {"hoard": list(self.rec.groups), "mint": list(self.mint_labels), "coin": list(self.coins),
                "ruler": list(self.rulers)}[t]

    def matches(self, t, x, filters, relations):
        return all(self.keep(t, x, f) != f.get("negate", False) for f in filters) \
            and all(self.holds(t, x, r) != r["negate"] for r in relations)

    def select(self, t, name, where):
        """The far-end entities of type t a relation may land on: named `name` (if given), meeting `where`."""
        return [y for y in self.candidates(t) if (not name or self.is_named(t, y, name))
                and self.matches(t, y, where["filters"], where["relations"])]

    def holds(self, t, x, rel):
        if not rel.get("where"):
            return self.related(t, x, rel["relation"], rel["other_type"], rel["other_name"])
        return any(self.link(t, x, rel["relation"], y) for y in self.select(rel["other_type"], rel["other_name"], rel["where"]))

    def link(self, t, x, r, y):
        """Is target x (of type t) linked to far-end entity y by r? One hop, one decider."""
        rec = self.rec
        in_hoard = lambda c, g: (self.coins[c].get("attributes") or {}).get("hoard") in self.group_ids(g)  # noqa: E731
        mint_of = lambda c: rec.labels(rec._mint_of(self.coins[c]) or "")  # noqa: E731
        ruler_of = lambda c: (self.coins[c].get("attributes") or {}).get("ruler")  # noqa: E731
        if r == "holds_coins_of":
            return rec.has(x, y) if t == "hoard" else rec.has(y, x)
        if r == "coin.hoard":
            return in_hoard(y, x) if t == "hoard" else in_hoard(x, y)
        if r == "coin.mint":
            return x in mint_of(y) if t == "mint" else y in mint_of(x)
        if r == "coin.ruler":
            return ruler_of(y) == x if t == "ruler" else ruler_of(x) == y
        return False

    def keep(self, target, x, f):
        a, op, v = f["attribute"], f["op"], f["value"]
        if a == "name":
            return op in ("eq", "contains") and self.is_named(target, x, v)
        if target == "hoard" and a == "findspot":
            if op == "contains":
                return self.rec.in_region(x, str(v))
            if op == "eq":
                return any(fold(s.strip()) == fold(str(v)) for s in self.rec.findspot[x].split(";"))
            return False
        val = self.attr(target, x, a)
        if isinstance(val, tuple) or val is None:
            n = year(v) if a in ("buried", "found", "struck") else (float(v) if isinstance(v, (int, float)) else year(v))
            return compare(val, op, n)
        s = fold(str(val))
        return s == fold(str(v)) if op == "eq" else bool(rx(str(v)).search(s)) if op == "contains" else False

    def run(self, q):
        if q is None:
            return None
        t, rec = q["target_type"], self.rec
        hit = [x for x in self.candidates(t) if self.matches(t, x, q["filters"], q["relations"])]
        name = (lambda x: rec.resolved[x]) if t == "hoard" else (lambda x: x) if t == "mint" else \
            (lambda x: f"{t}:{self.ents[x]['canonical_name']}")
        agg, over = q["aggregate"], q["aggregate_over"]
        if agg == "count":
            return len(hit)
        if agg == "none" or over is None:
            return sorted({name(x) for x in hit}) if agg == "none" else None
        # a count over a related type counts only the linked entities meeting that relation's `where`
        scoped = [set(self.select(over, r["other_name"], r["where"])) for r in q["relations"]
                  if r["other_type"] == over and r.get("where") and not r["negate"]]
        scope = set.intersection(*scoped) if scoped else None
        if t == "hoard" and over == "buried":                    # k2_t0 Records.superlative, earliest / latest
            cand = [g for g in hit if rec.burial[g]]
            if not cand:
                return None
            key = (lambda g: rec.burial[g][0]) if agg == "argmin" else (lambda g: -rec.burial[g][1])
            best = max(key(g) for g in cand)
            win = sorted({rec.resolved[g] for g in cand if key(g) == best})
            return win[0] if len(win) == 1 else win
        if t == "hoard" and over == "mint":                      # k2_t0 hoard-most-mints
            size = lambda g: len(rec.members[g] if scope is None else rec.members[g] & scope)  # noqa: E731
            return rec._argmax({rec.resolved[g]: size(g) for g in hit}) if agg == "argmax" else \
                self._argmin({rec.resolved[g]: size(g) for g in hit})
        if t == "mint" and over == "hoard":                      # k2_t0 mint-most-hoards
            pool = rec.groups if scope is None else scope
            score = collections.Counter({m: sum(1 for g in pool if rec.has(g, m)) for m in hit})
            return rec._argmax({m: v for m, v in score.items() if v}) if agg == "argmax" else self._argmin(dict(score))
        # any other (attribute or related type): generic, by the same tie rule
        def size(x):
            if over in {o for _, o, _ in EDGES.get(t, [])}:
                rels = [r for r, o, _ in EDGES[t] if o == over]
                pool = self.candidates(over) if scope is None else scope
                return sum(1 for y in pool if any(self.related(over, y, r, t, None) and self.link(t, x, r, y) for r in rels))
            iv = self.attr(t, x, over)
            return (iv[1] if agg == "argmax" else iv[0]) if isinstance(iv, tuple) else None
        sc = {name(x): v for x in hit if (v := size(x)) is not None}
        return rec._argmax(sc) if agg == "argmax" else self._argmin(sc)

    @staticmethod
    def _argmin(score):
        if not score:
            return None
        low = min(score.values())
        win = sorted(k for k, v in score.items() if v == low)
        return win[0] if len(win) == 1 else win


# ── reference queries (instrument check only; never shown to the model) ────
def reference(q):
    p, c = q["params"], q["class"]
    hc = lambda m, neg=False, inner=False: {"relation": "holds_coins_of", "other_type": "mint",  # noqa: E731
                                            "other_name": k2_bank.display_mint(m), "negate": neg, **({} if inner else {"where": None})}
    region = lambda r: [{"attribute": "findspot", "op": "contains", "value": r, "negate": False}]  # noqa: E731
    to_hoards = lambda where: {"relation": "holds_coins_of", "other_type": "hoard", "other_name": None,  # noqa: E731
                               "negate": False, "where": where}
    base = {"target_type": "hoard", "filters": [], "relations": [], "aggregate": "none", "aggregate_over": None}
    ms = p.get("mints", [])
    if c in ("intersection", "count"):
        return {**base, "relations": [hc(m) for m in ms], "aggregate": "count" if c == "count" else "none"}
    if c == "co-occurrence":                                      # mints in a hoard that holds M, other than M
        return {**base, "target_type": "mint",
                "filters": [{"attribute": "name", "op": "eq", "value": k2_bank.display_mint(ms[0]), "negate": True}],
                "relations": [to_hoards({"filters": [], "relations": [hc(ms[0], inner=True)]})]}
    if c == "constraint-date":
        way, y = p["burial"]
        return {**base, "filters": [{"attribute": "buried", "op": "lt" if way == "before" else "gt", "value": -y, "negate": False}],
                "relations": [hc(ms[0])]}
    if c == "constraint-region":
        return {**base, "filters": region(p["region"]), "relations": [hc(ms[0])]}
    if c == "negation":
        if p.get("region"):
            return {**base, "filters": region(p["region"]), "relations": [hc(ms[0], True)]}
        return {**base, "relations": [hc(ms[0]), hc(ms[1], True)]}
    if c == "superlative":
        if "mint-most-hoards" in q["id"]:
            rel = [to_hoards({"filters": region(p["region"]), "relations": []})] if p.get("region") else []
            return {**base, "target_type": "mint", "relations": rel, "aggregate": "argmax", "aggregate_over": "hoard"}
        f = region(p["region"]) if p.get("region") else []
        if "hoard-most-mints" in q["id"]:
            return {**base, "filters": f, "aggregate": "argmax", "aggregate_over": "mint"}
        rel = [hc(p["mint"])] if p.get("mint") else []
        return {**base, "filters": f, "relations": rel, "aggregate": "argmin" if p["way"] == "earliest" else "argmax",
                "aggregate_over": "buried"}
    return None                                                   # uncertainty: no attested case


# iteration 1 could not write these; iteration 2's `where` and filter `negate` express both
INEXPRESSIBLE = {}


# ── gold-complete records, from the bank's attested facts ──────────────────
def gold_records(bank, live):
    """A Records with the same interface, whose facts are the bank's attested ones (truth AND text;
    a pseudo-hoard by its text). The k2_t0 program over it must reproduce gold: that checks it."""
    fx = k2_bank.Fixture()
    F = k2_bank.Facts(fx)
    r = object.__new__(Records)
    r.mints, r.hoards, r.c2s, r.claims, r.ents, r.rels = live.mints, live.hoards, live.c2s, [], {}, []
    ok = lambda a: a["text"] == "T" and (a["truth"] == "T" or fx.role[k] == "pseudo")  # noqa: E731
    r.groups, r.members, r.findspot, r.burial, r.resolved = {}, {}, {}, {}, {}
    for k in fx.keys:
        g = fx.hid(k)
        r.groups[g] = [{"id": g, "canonical_name": fx.name[k], "attributes": {}}]
        r.members[g] = {m for m in fx.vocab if ok(F.contains(k, m))}
        r.findspot[g] = " ; ".join(reg for reg in k2_bank.REGIONS if ok(F.region(k, reg)))
        d = k2_bank.DATE_TEXT.get(k)
        r.burial[g] = (d[0], d[1]) if d else None
        r.resolved[g] = g
    return r


def same(a, b):
    return a == b


def diagnose(q, model_q, ref):
    if model_q is None:
        return "invalid output after retry"
    if ref is None:
        return "inexpressible: " + INEXPRESSIBLE.get(q["class"], "no reference query")
    why = []
    if model_q["target_type"] != ref["target_type"]:
        why.append(f"wrong target type ({model_q['target_type']} for {ref['target_type']})")
    mr = [(r["relation"], r["other_type"], fold(r["other_name"] or ""), r["negate"]) for r in model_q["relations"]]
    rr = [(r["relation"], r["other_type"], fold(r["other_name"] or ""), r["negate"]) for r in ref["relations"]]
    near = lambda a, b: a == b or bool(a) and bool(b) and (a in b or b in a)  # noqa: E731
    for x in rr:
        if x in mr:
            continue
        cand = [m for m in mr if near(m[2], x[2])]
        if not cand and x[2] and any(m[0] == x[0] and m[1] == x[1] and not m[2] for m in mr):
            why.append(f"name dropped: {x[0]}->{x[1]} with other_name null for {x[2]!r}")
        elif not cand:
            why.append(f"missing relation to {x[2] or 'any'}")
        elif cand[0][0] != x[0] or cand[0][1] != x[1]:
            why.append(f"wrong relation ({cand[0][0]}->{cand[0][1]} for {x[0]}->{x[1]})")
        elif cand[0][3] != x[3]:
            why.append("negation dropped" if x[3] else "spurious negation")
    for m in mr:
        if not any(near(m[2], x[2]) for x in rr) and not (not m[2] and any(m[0] == x[0] and m[1] == x[1] for x in rr)):
            why.append(f"extra relation ({m[0]} {m[2] or 'any'}{' NOT' if m[3] else ''})")
    for r in ref["relations"]:                                    # the far end's own conditions, one level down
        if not r.get("where"):
            continue
        twin = [m for m in model_q["relations"] if (m["relation"], m["other_type"], m["negate"]) == (r["relation"], r["other_type"], r["negate"])]
        if twin and not any(m.get("where") for m in twin):
            why.append(f"where dropped on {r['relation']}->{r['other_type']}")
        elif twin:
            sub = lambda w: {**w, "target_type": r["other_type"], "aggregate": "none", "aggregate_over": None}  # noqa: E731
            inner = diagnose(q, sub(next(m for m in twin if m.get("where"))["where"]), sub(r["where"]))
            if inner not in ("same query as the reference, different answer", "redundant relation only"):
                why.append(f"in where on {r['relation']}->{r['other_type']}: {inner}")
    for f in ref["filters"]:
        hit = [g for g in model_q["filters"] if g["attribute"] == f["attribute"]]
        if not hit:
            other = [g for g in model_q["filters"]]
            why.append(f"missing {f['attribute']} filter" + (f" (used {[(g['attribute'], g['op']) for g in other]})" if other else ""))
        elif hit[0]["op"] != f["op"]:
            why.append(f"{f['attribute']} op {hit[0]['op']} for {f['op']}")
        elif hit[0].get("negate", False) != f.get("negate", False):
            why.append(f"{f['attribute']} negation {'dropped' if f.get('negate') else 'spurious'}")
        elif f["attribute"] == "name" and not near(fold(str(hit[0]["value"])), fold(str(f["value"]))):
            why.append(f"name value {hit[0]['value']!r} for {f['value']!r}")
        elif f["attribute"] == "buried" and year(hit[0]["value"]) != f["value"]:
            why.append(f"buried value {hit[0]['value']!r} for {f['value']} (B.C. sign)")
        elif f["attribute"] == "findspot" and fold(str(hit[0]["value"])) != fold(f["value"]):
            why.append(f"findspot value {hit[0]['value']!r} for {f['value']!r}")
    for f in model_q["filters"]:
        if (f["attribute"], f["op"]) not in {(g["attribute"], g["op"]) for g in ref["filters"]} and \
                f["attribute"] not in {g["attribute"] for g in ref["filters"]}:
            why.append(f"extra filter ({f['attribute']} {f['op']} {f['value']!r})")
    if (model_q["aggregate"], model_q["aggregate_over"]) != (ref["aggregate"], ref["aggregate_over"]):
        why.append(f"aggregate {model_q['aggregate']}/{model_q['aggregate_over']} for {ref['aggregate']}/{ref['aggregate_over']}")
    if not why and model_q["relations"] != ref["relations"]:
        why.append("redundant relation only")
    return "; ".join(why) or "same query as the reference, different answer"


def main():
    global EDGES
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bank", type=pathlib.Path, default=HERE / "bank-k2-dev.toml")
    ap.add_argument("--json", type=pathlib.Path, default=HERE / "k2-query.json")
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--limit", type=int, default=None, help="first N questions only (smoke test)")
    ap.add_argument("--from-json", type=pathlib.Path, default=None,
                    help="rescore the queries stored in a previous k2-query.json (no model calls)")
    a = ap.parse_args()
    types, guidance = declared()
    ents, attrs, EDGES = grammar(types)
    sch = schema(ents, attrs, EDGES)
    jsonschema.Draft202012Validator.check_schema(sch)
    doc = documentation(types, guidance, ents, attrs, EDGES)
    bank = tomllib.loads(a.bank.read_text())
    qs = bank["questions"][:a.limit] if a.limit else bank["questions"]
    for q in qs:
        q.setdefault("form", None)
    live = Records(ATLAS, bank, CORPUS)
    ideal = gold_records(bank, live)
    ex_live, ex_ideal = Executor(live, live.ents), Executor(ideal)

    # instrument checks, before any model output is trusted
    checks = {"reference_reproduces_program": [], "program_on_gold_records_vs_gold": []}
    for q in qs:
        ref = reference(q)
        if ref is not None:
            jsonschema.validate(ref, sch)
            for name, rec, ex in (("live", live, ex_live), ("gold", ideal, ex_ideal)):
                if ex.run(ref) != rec.answer(q):
                    checks["reference_reproduces_program"].append(f"{q['id']} on {name}: {ex.run(ref)!r} != {rec.answer(q)!r}")
        s = score_records(q, ideal.answer(q), ideal)
        if s.get("f1", s.get("exact")) < 1.0:
            checks["program_on_gold_records_vs_gold"].append(f"{q['id']}: {s.get('f1', s.get('exact')):.2f}")
    print(f"instrument: reference query != program on {len(checks['reference_reproduces_program'])} cases; "
          f"program on gold-complete records < 1.0 on {len(checks['program_on_gold_records_vs_gold'])} questions")
    for k, v in checks.items():
        for x in v:
            print(f"  {k}: {x}")

    runs = []
    if a.from_json:
        prev = {r["id"]: r for r in json.loads(a.from_json.read_text())["questions"]}
        keep = lambda r, k: {"query": r[k], "raw": None, "seconds": r["seconds"] if k == "query" else None,  # noqa: E731
                             "attempts": r["attempts"]}
        runs = [[keep(prev[q["id"]], "query") for q in qs], [keep(prev[q["id"]], "run2_query") for q in qs]]
        a.runs = 0
        print(f"rescoring stored queries from {a.from_json.name} (no model calls)")
    for run in range(a.runs):
        out = []
        for q in qs:
            mq, raw, secs, att = parse(doc, sch, q["question"])
            out.append({"query": mq, "raw": raw, "seconds": secs, "attempts": att})
            print(f"  run {run + 1} {q['id']:40} {secs:5.1f}s {json.dumps(mq, ensure_ascii=False)[:150]}", flush=True)
        runs.append(out)

    rows = []
    for i, q in enumerate(qs):
        r1 = runs[0][i]
        ref = reference(q)
        got, prog = ex_live.run(r1["query"]), live.answer(q)
        got_g, prog_g = ex_ideal.run(r1["query"]), ideal.answer(q)
        vac = prog in ([], 0, None)
        e2e = score_records(q, got, live) if r1["query"] is not None else score_records(q, [] if q["answer_type"] in LISTS else None, live)
        agree, agree_g = same(got, prog), same(got_g, prog_g)
        rows.append({
            "id": q["id"], "class": q["class"], "question": q["question"],
            "query": r1["query"], "reference": ref, "attempts": r1["attempts"], "seconds": r1["seconds"],
            "deterministic": all(json.dumps(r[i]["query"], sort_keys=True) == json.dumps(r1["query"], sort_keys=True) for r in runs[1:]),
            "run2_query": runs[1][i]["query"] if len(runs) > 1 else None,
            "executed": got, "program": prog, "parse_agree": agree, "vacuous": vac and agree,
            "executed_on_gold_records": got_g, "program_on_gold_records": prog_g, "parse_agree_gold_records": agree_g,
            "end_to_end": {k: e2e[k] for k in ("f1", "precision", "recall", "made_up", "exact") if k in e2e},
            "expressible": ref is not None,
            "coincidental": (agree or agree_g) and ref is None,
            "query_correct": ref is not None and agree and agree_g,
            "diagnosis": diagnose(q, r1["query"], ref) if not (agree and agree_g) or ref is None
            else (lambda d: d if d == "redundant relation only" else None)(diagnose(q, r1["query"], ref)),
        })
    t0j = json.loads((HERE / "k2-t0.json").read_text()) if (HERE / "k2-t0.json").exists() else {"per_class": {}, "questions": []}
    t0 = dict(t0j["per_class"])
    ids = {r["id"] for r in rows}
    t0q = [r for r in t0j["questions"] if r["id"] in ids]
    if t0q:                                                       # the ALL row, question-weighted as here
        m = lambda xs: round(sum(xs) / len(xs), 3)  # noqa: E731
        t0["ALL"] = {"records": m([r["records"].get("f1", r["records"].get("exact")) for r in t0q]),
                     "rag@20": m([r["rag"]["20"].get("f1", r["rag"]["20"].get("exact")) for r in t0q])}
    order = list(dict.fromkeys(r["class"] for r in rows))
    mean = lambda xs: round(sum(xs) / len(xs), 3) if xs else None  # noqa: E731
    e2e = lambda r: r["end_to_end"].get("f1", r["end_to_end"].get("exact"))  # noqa: E731
    per = {}
    for cls in order + ["ALL"]:
        rs = [r for r in rows if cls == "ALL" or r["class"] == cls]
        per[cls] = {"n": len(rs), "parse_agree": mean([r["parse_agree"] for r in rs]),
                    "parse_agree_non_vacuous": mean([r["parse_agree"] and not r["vacuous"] for r in rs]),
                    "parse_agree_gold_records": mean([r["parse_agree_gold_records"] for r in rs]),
                    "expressible": sum(r["expressible"] for r in rs),
                    "query_correct": mean([r["query_correct"] for r in rs]),
                    "query_correct_of_expressible": mean([r["query_correct"] for r in rs if r["expressible"]]),
                    "coincidental_agreements": sum(r["coincidental"] for r in rs),
                    "end_to_end": mean([e2e(r) for r in rs]),
                    "made_up": sum(len(r["end_to_end"].get("made_up", [])) for r in rs),
                    "deterministic": sum(r["deterministic"] for r in rs),
                    "records_ceiling": t0.get(cls, {}).get("records"), "rag@20": t0.get(cls, {}).get("rag@20")}
    bar = per["ALL"]["parse_agree"]
    verdict = "build (>= 0.80)" if bar >= 0.80 else "rethink (< 0.50)" if bar < 0.50 else "one iteration (0.50-0.80)"
    out = {"model": MODEL, "endpoint": CHAT, "corpus": CORPUS, "runs": len(runs), "verdict_on_parse_agree": verdict,
           "constraint": {"field": "response_format json_schema",
                          "enforced_at": ["sovereign/crates/sovereign-serving-host/src/inference_adapter.rs:415",
                                          "sovereign/crates/sovereign-serving-host/src/inference_adapter.rs:1000",
                                          "sovereign/crates/sovereign-inference/src/embedded/sampler.rs:342"],
                          "thinking_off": ["chat_template_kwargs.enable_thinking=false (inference_adapter.rs:544)",
                                           "think_budget=0 (inference_adapter.rs:379)"],
                          "outputs_failing_validation": sum(r["attempts"] > 1 for r in rows)},
           "inexpressible": INEXPRESSIBLE, "instrument_checks": checks, "per_class": per,
           "schema": sch, "system": SYSTEM, "documentation": doc,
           "prompt_sha256": hashlib.sha256((SYSTEM + doc).encode()).hexdigest(), "questions": rows}
    a.json.write_text(json.dumps(out, indent=1, ensure_ascii=False))

    print(f"\n{'class':18} {'n':>2} {'parse':>6} {'non-vac':>7} {'goldrec':>7} {'expr':>4} {'correct':>7} {'coinc':>5} "
          f"{'e2e':>6} {'madeup':>6} {'det':>4} {'rec-ceil':>8} {'rag@20':>7}")
    for cls in order + ["ALL"]:
        c = per[cls]
        print(f"{cls:18} {c['n']:2} {c['parse_agree']:6} {c['parse_agree_non_vacuous']:7} {c['parse_agree_gold_records']:7} "
              f"{c['expressible']:4} {c['query_correct']:7} {c['coincidental_agreements']:5} "
              f"{c['end_to_end']:6} {c['made_up']:6} {c['deterministic']:4} {str(c['records_ceiling']):>8} {str(c['rag@20']):>7}")
    print(f"\nverdict on parse agreement {bar}: {verdict}; outputs failing validation: "
          f"{out['constraint']['outputs_failing_validation']}")
    print("\ndisagreements (atlas records or gold-complete records):")
    for r in rows:
        if r["diagnosis"]:
            print(f"  {r['id']:40} live={'Y' if r['parse_agree'] else 'n'} gold={'Y' if r['parse_agree_gold_records'] else 'n'}"
                  f"{' COINCIDENTAL' if r['coincidental'] else ''}  {r['diagnosis']}")
    print(f"-> {a.json.name}")


EDGES = {}

if __name__ == "__main__":
    main()
