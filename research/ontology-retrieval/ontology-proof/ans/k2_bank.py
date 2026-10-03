#!/usr/bin/env python3
"""K2 semantic-query bank over the dev fixture `ft-ans-dev-b` (feature-fidelity campaign,
.sovereign/features/feature-fidelity/campaign.md, Ontology: "New instrument, K2 semantic queries").

    python3 k2_bank.py             # -> bank-k2-dev.toml; prints per-class counts and gold spans
    python3 k2_bank.py --audit     # also prints every mint mention per hoard with its verdict

THE RULE, fixed 2026-10-03 before any arm or T0 number existed for K2. K1's lesson binds it:
a bank built from catalogue truth was void because 82% of its facts were not in the text.

* POPULATION. A hoard is in the fixture when a section of ft-ans-dev-b is its (the O-PPR T0
  scratch study's sim.py rule, copied in `sections_for`): a chapter title citing its IGCH number;
  or, citing none, whose last segment names it (make_bank's NAME), carries no year but its
  discovery year, from a work published no earlier than the discovery. An inline entry heading
  inside a section ("Jasna Poljana ( IGCH 777)" in the Paeonia section) closes the open hoard:
  text after it is the inline hoard's. A section that is no truth hoard's ("Asia Minor 1970") is
  a pseudo-hoard (no truth row, so only what its text states is possible for it). GOLD answers
  are DEV hoards only (even IGCH, fixture_dev.dev); holdout and pseudo-hoards only ever yield
  NEUTRAL items.
* A FACT is GOLD only when truth/hoards.json AND the hoard's own text both state it:
    contains(H,M)  truth lists M for H, and a chunk of H's text names M. The matcher is
                   make_bank's (fold / rx / short) made CASE-SENSITIVE, because "Side" is a mint
                   and an English word ("only one side of each coin"); widened by ALIASES, the
                   text's own spellings (Sardes, Magnesia, Ake) and adjectives (Sidonian ...);
                   BIBLIO masked (Thompson's book "Sardes and Miletus" heads every NS 19 entry);
                   NOT_CONTENT mentions dropped and NEGATED ones read as absence; a fact resting
                   only on an IMPLIED mention is kept but flagged `implied`. Every quote in those
                   tables is verified in its chunk or the run is REFUSED.
    region(H,R)    REGION_TRUTH (from the IGCH findspot) and a REGION_TEXT quote in H's text.
                   "Asia Minor mints/material" is NOT a findspot, hence quotes, not a word match.
    burial(H)      the truth `deposit` and every DATE_TEXT statement for H fall on the same side
                   of the cutoff; an interval touching the cutoff is ambiguous.
    uncertain(H,M) truth `uncertain` and a HEDGE within HEDGE_WINDOW chars of M in H's text.
    lacks(H,M)     closed over H's attested text (no content mention of M) and truth lacks M;
                   its attesting chunks are ALL of H's chunks (`all_chunks`): absence needs a full read.
* NEUTRAL: an item not gold that truth or text leaves possible (text-only, truth-only, holdout
  or pseudo hoard, one side silent, an interval across the cutoff). Scorers ignore neutrals.
  A neutral is VISIBLE when every fact it needs is text-possible: a text reader would list it.
* Counts are kept only with no visible neutral (truth-only neutrals flag open_world_sensitive).
  Superlatives need >= 2 candidates and the same strict winner under the gold reading and the
  text-possible reading; all are flagged open_world_sensitive. Negations are always flagged, and
  must exclude something: the negated mint is named by the text in >= 1 in-scope hoard and in
  at least half as many as the gold ("contain no coins of a rare mint" is trivia).
* SELECTION per class: every candidate the templates produce over the attested vocabulary is
  judged; lists kept if gold >= 1 and visible neutrals <= gold, ranked by (pre-registered
  template first, gold - visible neutral, gold, id), a repeat of a kept gold set skipped; <= 8.
* Every fact records its attesting chunk ids (chunks.lance `id`), which K2 T0's RAG ceiling uses.
"""
import argparse, collections, itertools, json, pathlib, re, sys, unicodedata

import lance

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from make_bank import fold, rx, short  # noqa: E402  (one matcher: the bank's)
from fixture_dev import dev  # noqa: E402  (one split: even IGCH)

CORPUS = "ft-ans-dev-b"
IDX = pathlib.Path.home() / ".svrnmesh/indexes" / CORPUS
OUT = HERE / "bank-k2-dev.toml"
CAP = 8
CUTOFFS = (310,)
HEDGE = re.compile(r"\?|probabl|possibl|uncertain|doubt|tentativ|perhaps|may be")
HEDGE_WINDOW = 60

# The text's spellings of a truth label (short(label) is always tried first).
ALIASES = {
    "Sardis": ["Sardes", "Sardian"], "Magnesia ad Maeandrum": ["Magnesia", "Magnesian"],
    "Ptolemais-Ake": ["Ake"], "Sidon": ["Sidonian"], "Colophon": ["Colophonian"],
    "Miletus": ["Milesian"], "Abydus": ["Abydene"], "Lampsacus": ["Lampsacene"],
}
DISPLAY = {"Sardis": "Sardes", "Magnesia ad Maeandrum": "Magnesia", "Ptolemais-Ake": "Ake"}
# Book titles, not hoard contents (Thompson, Alexander's Drachm Mints I: Sardes and Miletus;
# Newell, The Dated Alexander Coinage of Sidon and Ake).
BIBLIO = [r"Sardes and Miletus", r"Sidon and Ake"]
# (chunk, quote, why): a mint named here is not named as this hoard's content.
NOT_CONTENT = [
    (41, "one part had been sent to Alexandria", "Alexandria the city the find was sold in"),
    (43, "whether struck in Macedonia or distant Babylon", "general remark on style"),
    (45, "emanating from the mint at Egyptian Alexandria", "style discussion"),
    (45, "the coins of Byblus bear the abbreviated name", "style discussion"),
    (45, "while the Sidonian issues are struck from fixed", "style discussion"),
    (45, "The Alexander issues of Babylon are conspicuous", "style discussion"),
    (45, "For the newly opened mint at Alexandria in Egypt", "style discussion"),
    (45, "used as models in Byblus, Berytus Sidon, Ake, Citium, Amathus", "mints copying a style"),
    (46, "used as models in Byblus, Berytus Sidon, Ake, Citium, Amathus", "mints copying a style"),
    (14, "For Miletus the only tetradrachm issue of this early period not represented in Demanhur",
     "footnote about Demanhur, in the Kuft entry"),
    (15, "Listed by Pfeiler as Miletus ?", "orphan footnote; its hoard is not identifiable"),
    (22, "is not Lampsacus", "a rejected attribution"),
    (30, "have a few identifiable coins of Lampsacus and Abydus", "about 'other hoards laid away ca. 300'"),
    (6, "Magnesia, Mylasa, Mytilene and Rhodes. In all probability Abydus, Ephesus and Teos or Phocaea",
     "mints striking late Philips, not this hoard's contents"),
    (34, "had been found in Elis", "a findspot, not a mint"),
    (34, "Elean issues", "an adjective, not a truth label"),
    (34, "similar to ones already found at Epidaurus, Kyparissia (late fourth century), Patras, Sophikon, Sparta",
     "other hoards' findspots"),
    (36, "M. Pierre Saroglos of Athens", "a collector's city"),
]
# (chunk, quote, why): content only by implication; facts resting on these alone carry
# `implied = true` so a scorer can report them apart.
IMPLIED = [
    (49, "In the cases of Tarsus, Myriandrus, Sidon, and Ake the writer",
     "a note on the numbering of the Demanhur list implies these mints are in it; the list is not in the fixture"),
]
# (chunk, quote, [labels]): the text says the hoard has NONE of these.
NEGATED = [
    (2, "all seven Asia Minor mints except Colophon", ["Colophon"]),
    (11, "There is nothing from Teos", ["Teos"]),
    (18, "it had no tetradrachms of Lampsacus, Abydus, Colophon, Magnesia and Teos",
     ["Lampsacus", "Abydus", "Colophon", "Magnesia ad Maeandrum", "Teos"]),
    (34, "the issues of Corinth are conspicuous by their absence", ["Corinth"]),
]
# From the IGCH findspot (truth/hoards.json `findspot`), by hoard key.
REGION_TRUTH = {
    1664: {"Egypt"}, 1668: {"Egypt"}, 1670: {"Egypt"}, 1678: {"Egypt"},
    1436: {"Asia Minor"}, 1437: {"Asia Minor"}, 1438: {"Asia Minor"}, 1441: {"Asia Minor"}, 1444: {"Asia Minor"},
    176: {"Peloponnese"}, 774: {"Romania"}, 958: {"Romania"}, 866: {"Ukraine"}, 777: {"Bulgaria"},
    801: {"Greece"}, 1472: {"Cyprus"}, 1516: {"Syria"}, 410: {"Paeonia"},
}
REGIONS = {"Egypt": "Egypt", "Asia Minor": "Asia Minor", "Peloponnese": "the Peloponnese"}
# (hoard key, region) -> [(chunk, quote)]: the hoard's own text places it there.
REGION_TEXT = {
    (1664, "Egypt"): [(37, "a hoard of these coins found also in Egypt"), (35, "mostly purchased in Egypt shortly after")],
    (1668, "Egypt"): [(27, "Egypt 1912: IGCH 1668")],
    (1678, "Egypt"): [(16, "the heavier coins from outside Egypt"), (16, "could scarcely have reached Egypt by 305")],
    (1444, "Asia Minor"): [(15, "Asia Minor 1961 ( IGCH 1444)"), (15, "burial took place somewhere in northwestern Asia Minor"),
                           (30, "Asia Minor 1961: IGCH 1444")],
    (1438, "Asia Minor"): [(25, "Asia Minor 1964: IGCH 1438")],
    (176, "Peloponnese"): [(34, "a typical third century Peloponnesian hoard"), (34, "the hoard had been found in Elis")],
    (1437, "Asia Minor"): [(8, "Asia Minor 1964 ( IGCH 1437)"), (24, "Asia Minor 1964: IGCH 1437")],
    (1441, "Asia Minor"): [(2, "Asia Minor 1964 ( IGCH 1441)"), (20, "Asia Minor 1964: IGCH 1441")],
    ("sec_00012", "Asia Minor"): [(18, "If the discovery was made in Asia Minor")],
}
# hoard key -> (earliest, latest) burial year B.C. the text allows, [(chunk, quote)].
# "c. X" is read as X+-2; a stated revision widens the interval to cover both readings.
DATE_TEXT = {
    1664: (318, 318, [(13, "The burial date of 318 B.C.")]),
    774: (320, 312, [(1, "The suggested burial date, c. 320 B.C., may be too early")]),
    1444: (302, 298, [(15, "A burial date c. 300 B.C. is established")]),
    1472: (302, 298, [(5, "Price and Le Rider agree on a burial c. 300 B.C.")]),
    1516: (307, 295, [(17, "his burial date of c. 305 B.C."), (17, "a deposit of c. 300 or even slightly later"),
                      (28, "interred ca. 300 at the earliest and probably a few years later")]),
    1678: (307, 295, [(16, "until c. 305 B.C."), (16, "the date should be lowered to c. 300 or even later")]),
    1670: (312, 303, [(14, "provide the evidence for interment c. 310"), (14, "Nash prefers the later date, c. 305")]),
    410: (316, 312, [(3, "A burial date shortly after 316/5 B.C.")]),
    866: (230, 218, [(7, "according to Seyrig")]),
    176: (300, 201, [(34, "a typical third century Peloponnesian hoard")]),
    1437: (323, 319, [(10, "burial date c. 321 B.C.")]),
    1441: (317, 313, [(2, "burial date of c. 315 in IGCH")]),
    777: (323, 313, [(4, "points to burial after c. 315 B.C.")]),
}
INLINE_ENTRY = re.compile(r"(?m)^[^\n›|]{2,60}?\(\s*IGCH (\d+)\)\s*$")


# ── the fixture ────────────────────────────────────────────────────────────
def name_of(h):
    return re.split(r"[,(]", h["findspot"] or "")[0].strip().strip('"“”')  # make_bank.k1's NAME


def sections_for(h, chapters):
    """sim.py hoard_sections, one hoard at a time (the O-PPR T0 study's rule, unchanged)."""
    name, disc, out = name_of(h), re.search(r"\d{4}", h.get("discovery") or ""), []
    for c in chapters:
        t = c["title"]
        cited = [int(n) for n in re.findall(r"IGCH\s*(\d+)", t)]
        if cited:
            if h["igch"] in cited:
                out.append(c["id"])
            continue
        last = t.rsplit(" › ", 1)[-1]
        if not name or not rx(name).search(fold(last)):
            continue
        yrs = re.findall(r"(?<!\d)(1[89]\d\d|20\d\d)(?!\d)", last)
        if yrs and (not disc or any(y != disc.group(0) for y in yrs)):
            continue
        pub = re.search(r"\((\d{4})\)", t.split(" › ", 1)[0])
        if disc and pub and int(pub.group(1)) < int(disc.group(0)):
            continue
        out.append(c["id"])
    return out


def deaccent(s):
    """make_bank.fold without the casefold, one char per char (offsets survive): the
    case-sensitive half of the matcher."""
    out = []
    for c in s:
        d = [x for x in unicodedata.normalize("NFKD", c) if not unicodedata.combining(x)]
        out.append(d[0] if len(d) == 1 else c)
    return "".join(out)


def crx(name):
    n = re.escape(deaccent(name))
    return re.compile(rf"(?<!\w)(?:{n}|{n.upper()})(?!\w)")


def names_for(label):
    return list(dict.fromkeys([short(label)] + ALIASES.get(label, [])))


def display_mint(label):
    return DISPLAY.get(label, short(label))


class Fixture:
    def __init__(self):
        self.chapters = json.loads((IDX / "chapters.json").read_text())["chapters"]
        t = lance.dataset(str(IDX / "chunks.lance")).to_table(columns=["id", "content"]).to_pydict()
        self.chunk = dict(zip(t["id"], t["content"]))
        self.truth = {h["igch"]: h for h in json.loads((HERE / "truth/hoards.json").read_text())["hoards"]}
        self.verify_quotes()
        # hoard key -> [(chunk, start, end)] : the spans of chunk text that are this hoard's.
        self.spans = collections.defaultdict(list)
        owners = collections.defaultdict(list)
        for n, h in self.truth.items():
            for s in sections_for(h, self.chapters):
                owners[s].append(n)
        for c in self.chapters:
            own = owners.get(c["id"]) or [c["id"]]          # a section no truth row claims: pseudo-hoard
            if len(own) > 1:
                sys.exit(f"REFUSED: section {c['id']} maps to several hoards {own}")
            cur = own[0]
            for cid in c["chunk_ids"]:
                text, at = self.chunk[cid], 0
                for m in INLINE_ENTRY.finditer(text):
                    n = int(m.group(1))
                    if m.start() > 0 and n != cur and n in self.truth:
                        self.spans[cur].append((cid, at, m.start())); cur, at = n, m.start()
                self.spans[cur].append((cid, at, len(text)))
        self.keys = sorted(self.spans, key=str)
        self.role = {k: ("pseudo" if isinstance(k, str) else "dev" if dev(k) else "holdout") for k in self.keys}
        self.title = {c["id"]: c["title"] for c in self.chapters}
        self.vocab = sorted({m for h in self.truth.values() for m in h["mints"] + (h.get("uncertain") or {}).get("mints", [])}
                            - {"Uncertain value"})
        self.mentions = {k: self._mentions(k) for k in self.keys}
        self._names()

    def verify_quotes(self):
        quotes = [(c, q) for c, q, _ in NOT_CONTENT + IMPLIED] + [(c, q) for c, q, _ in NEGATED]
        quotes += [x for v in REGION_TEXT.values() for x in v] + [x for v in DATE_TEXT.values() for x in v[2]]
        bad = [(c, q) for c, q in quotes if q not in self.chunk[c]]
        if bad:
            sys.exit(f"REFUSED: quotes not in their chunks: {bad}")

    def text_of(self, k):
        return [(cid, self.chunk[cid][a:b], a) for cid, a, b in self.spans[k]]

    def chunks_of(self, k):
        return sorted({cid for cid, _, _ in self.spans[k]})

    def _mentions(self, k):
        """label -> [(chunk, verdict, context)], verdict in content / not_content / negated."""
        out = collections.defaultdict(list)
        for cid, text, off in self.text_of(k):
            masked = deaccent(text)
            for b in BIBLIO:
                masked = re.sub(b, lambda m: " " * len(m.group(0)), masked)
            full = self.chunk[cid]
            drop = [(full.find(q), full.find(q) + len(q), why) for c, q, why in NOT_CONTENT if c == cid]
            neg = [(full.find(q), full.find(q) + len(q), set(ls)) for c, q, ls in NEGATED if c == cid]
            imp = [(full.find(q), full.find(q) + len(q)) for c, q, _ in IMPLIED if c == cid]
            assert len(deaccent(full)) == len(full)
            for label in self.vocab:
                for nm in names_for(label):
                    for m in crx(nm).finditer(masked):
                        a = off + m.start()
                        ctx = full[max(0, a - 50):a + len(nm) + 40].replace("\n", " ⏎ ")
                        if any(s <= a < e and label in ls for s, e, ls in neg):
                            out[label].append((cid, "negated", ctx))
                        elif any(s <= a < e for s, e, _ in drop):
                            out[label].append((cid, "not_content", ctx))
                        elif any(s <= a < e for s, e in imp):
                            out[label].append((cid, "implied", ctx))
                        else:
                            out[label].append((cid, "content", ctx))
        return out

    def _names(self):
        base = {}
        for k in self.keys:
            if isinstance(k, str):
                base[k] = self.title[k].rsplit(" › ", 1)[-1]
            else:
                h, nm = self.truth[k], name_of(self.truth[k])
                base[k] = f"{nm} {h['discovery']}" if nm in ("Asia Minor", "Egypt") and h["discovery"] else nm
        dup = {b for b, n in collections.Counter(base.values()).items() if n > 1}
        self.name, self.match = {}, {}
        for k in self.keys:
            igch = None if isinstance(k, str) else k
            self.name[k] = f"{base[k]} (IGCH {igch})" if base[k] in dup else base[k]
            self.match[k] = ([f"igch {igch}"] if igch else []) + ([] if base[k] in dup else [fold(base[k])])

    def hid(self, k):
        return k if isinstance(k, str) else f"igch{k:04d}"


# ── facts: (text status, truth status, attesting chunks) ───────────────────
# text: T stated, F stated absent/false, A ambiguous (interval across the cutoff), ? silent.
# truth: T, F, ? (no value / uncertain / pseudo-hoard).
class Facts:
    def __init__(self, fx):
        self.fx = fx

    def contains(self, k, label):
        ms = self.fx.mentions[k].get(label, [])
        content = sorted({c for c, v, _ in ms if v in ("content", "implied")})
        implied = bool(content) and not any(v == "content" for _, v, _ in ms)
        text = "T" if content else "F" if any(v == "negated" for _, v, _ in ms) else "?"
        if isinstance(k, str):
            truth = "?"
        else:
            h = self.fx.truth[k]
            truth = "T" if label in h["mints"] else "?" if label in (h.get("uncertain") or {}).get("mints", []) else "F"
        return {"fact": f"contains {self.fx.hid(k)} {display_mint(label)}", "text": text, "truth": truth, "chunks": content,
                "implied": implied}

    def lacks(self, k, label):
        c = self.contains(k, label)
        flip = {"T": "F", "F": "T", "?": "T"}           # closed world over the attested text
        neg = sorted({cid for cid, v, _ in self.fx.mentions[k].get(label, []) if v == "negated"})
        truth = {"T": "F", "F": "T", "?": "?"}[c["truth"]]
        return {"fact": f"lacks {self.fx.hid(k)} {display_mint(label)}", "text": flip[c["text"]], "truth": truth,
                "chunks": neg or self.fx.chunks_of(k), "closed_world": not neg}

    def region(self, k, r):
        q = REGION_TEXT.get((k, r), [])
        truth = "?" if isinstance(k, str) or k not in REGION_TRUTH else "T" if r in REGION_TRUTH[k] else "F"
        return {"fact": f"region {self.fx.hid(k)} {r}", "text": "T" if q else "?", "truth": truth,
                "chunks": sorted({c for c, _ in q})}

    def burial(self, k, way, y):
        """way 'before' = buried earlier than y B.C. (year B.C. > y); 'after' = later."""
        def side(early, late):
            if way == "before":
                return "T" if late > y else "F" if early < y else "A"
            return "T" if early < y else "F" if late > y else "A"
        d = DATE_TEXT.get(k)
        text = side(d[0], d[1]) if d else "?"
        dep = None if isinstance(k, str) else self.fx.truth[k]["deposit"]
        n = int(re.match(r"(\d+)", dep).group(1)) if dep else None
        truth = "?" if n is None or n == y else side(n, n)
        return {"fact": f"burial {self.fx.hid(k)} {way} {y} B.C.", "text": text, "truth": truth,
                "chunks": sorted({c for c, _ in d[2]}) if d else []}

    def date(self, k):
        d = DATE_TEXT.get(k)
        return {"fact": f"burial {self.fx.hid(k)} date", "chunks": sorted({c for c, _ in d[2]}) if d else []}


def judge(fx, k, atoms):
    """gold / neutral / false for one item whose facts are `atoms` (a conjunction)."""
    if any(a["text"] == "F" and a["truth"] != "T" or a["truth"] == "F" and a["text"] != "T" for a in atoms):
        return "false", None
    if fx.role[k] == "pseudo" and any(a["text"] == "?" for a in atoms):
        return "false", None                            # no truth row: only its text speaks for it
    if fx.role[k] == "dev" and all(a["text"] == "T" and a["truth"] == "T" for a in atoms):
        return "gold", None
    visible = all(a["text"] in ("T", "A") for a in atoms)
    why = ("holdout hoard" if fx.role[k] == "holdout" else "pseudo-hoard (no truth row)" if fx.role[k] == "pseudo"
           else "; ".join(f"{a['fact']}: text {a['text']} truth {a['truth']}" for a in atoms
                          if not (a["text"] == "T" and a["truth"] == "T")))
    return "neutral", {"visible": visible, "why": why}


def fact_out(a):
    """A closed-world absence is read off the hoard's whole text: every chunk is needed."""
    return ({k: a[k] for k in ("fact", "chunks")} | ({"all_chunks": True} if a.get("closed_world") else {})
            | ({"implied": True} if a.get("implied") else {}))


# ── classes ────────────────────────────────────────────────────────────────
def hoard_list(fx, F, qid, cls, question, atoms_of, params, extra=None):
    gold, neutral = [], []
    for k in fx.keys:
        atoms = atoms_of(k)
        v, n = judge(fx, k, atoms)
        if v == "gold":
            gold.append({"id": fx.hid(k), "name": fx.name[k], "match": fx.match[k], "witnesses": [[fact_out(a) for a in atoms]]})
        elif v == "neutral":
            neutral.append({"id": fx.hid(k), "name": fx.name[k], "match": fx.match[k], **n})
    return {"id": qid, "class": cls, "question": question, "answer_type": "hoards", "gold_items": gold,
            "neutral_items": neutral, "params": params, **(extra or {})}


def candidates(fx, F):
    V = [m for m in fx.vocab if any(F.contains(k, m)["text"] == "T" and F.contains(k, m)["truth"] == "T"
                                    for k in fx.keys if fx.role[k] == "dev")]
    R = [r for r in REGIONS if any(F.region(k, r)["text"] == "T" and F.region(k, r)["truth"] == "T"
                                   for k in fx.keys if fx.role[k] == "dev")]
    slug = lambda s: fold(display_mint(s) if s in fx.vocab else s).replace(" ", "-")  # noqa: E731
    out = collections.defaultdict(list)
    for a, b in itertools.combinations(V, 2):
        out["intersection"].append(hoard_list(
            fx, F, f"k2-int-{slug(a)}-{slug(b)}", "intersection",
            f"Which hoards contain coins of both {display_mint(a)} and {display_mint(b)}?",
            lambda k: [F.contains(k, a), F.contains(k, b)], {"mints": [a, b]}))
    for y, way, m in itertools.product(CUTOFFS, ("before", "after"), V):
        out["constraint-date"].append(hoard_list(
            fx, F, f"k2-date-{way}{y}-{slug(m)}", "constraint-date",
            f"Which hoards buried {way} {y} B.C. contain coins of {display_mint(m)}?",
            lambda k: [F.burial(k, way, y), F.contains(k, m)], {"mints": [m], "burial": [way, y]}))
    for r, m in itertools.product(R, V):
        out["constraint-region"].append(hoard_list(
            fx, F, f"k2-region-{slug(r)}-{slug(m)}", "constraint-region",
            f"Which hoards found in {REGIONS[r]} contain coins of {display_mint(m)}?",
            lambda k: [F.region(k, r), F.contains(k, m)], {"mints": [m], "region": r}))
    # A negation must exclude something: the negated mint is named by the text in >= 1 in-scope
    # hoard, and in at least half as many as the gold (else "none of a rare mint" is trivia).
    def excludes(q, in_scope, m):
        named = sum(1 for k in fx.keys if fx.role[k] == "dev" and in_scope(k) and F.contains(k, m)["text"] == "T")
        if not named or 2 * named < len(q["gold_items"]):
            q["invalid"] = "negated mint rare in scope"
        return q
    gold = lambda a: a["text"] == "T" and a["truth"] == "T"  # noqa: E731
    for r, m in itertools.product(R, V):
        q = hoard_list(fx, F, f"k2-neg-{slug(r)}-{slug(m)}", "negation",
                       f"Which hoards found in {REGIONS[r]} contain no coins of {display_mint(m)}?",
                       lambda k: [F.region(k, r), F.lacks(k, m)], {"mints": [m], "region": r},
                       {"open_world_sensitive": True})
        out["negation"].append(excludes(q, lambda k: gold(F.region(k, r)), m))
    for a, b in itertools.permutations(V, 2):
        q = hoard_list(fx, F, f"k2-neg-{slug(a)}-not-{slug(b)}", "negation",
                       f"Which hoards contain coins of {display_mint(a)} but none of {display_mint(b)}?",
                       lambda k: [F.contains(k, a), F.lacks(k, b)], {"mints": [a, b]},
                       {"open_world_sensitive": True, "template": 1})
        out["negation"].append(excludes(q, lambda k: gold(F.contains(k, a)), b))
    # count: the size of a hoard list, kept only when no neutral is visible to a text reader
    for m in V:
        q = hoard_list(fx, F, f"k2-count-{slug(m)}", "count", f"How many hoards contain coins struck at {display_mint(m)}?",
                       lambda k: [F.contains(k, m)], {"mints": [m]})
        out["count"].append(as_count(q))
    for a, b in itertools.combinations(V, 2):
        q = hoard_list(fx, F, f"k2-count-{slug(a)}-{slug(b)}", "count",
                       f"In how many hoards do coins of {display_mint(a)} and {display_mint(b)} occur together?",
                       lambda k: [F.contains(k, a), F.contains(k, b)], {"mints": [a, b]}, {"template": 1})
        out["count"].append(as_count(q))
    out["superlative"] += superlatives(fx, F, V, R, slug)
    for m in V:
        out["co-occurrence"].append(cooccurrence(fx, F, m, slug))
    out["uncertainty"] += uncertainty(fx, F)
    return out, V, R


def as_count(q):
    vis = [n for n in q["neutral_items"] if n["visible"]]
    q.update(answer_type="int", answer=len(q["gold_items"]),
             needed_facts=[f for g in q["gold_items"] for f in g["witnesses"][0]],
             open_world_sensitive=bool(q["neutral_items"]),
             invalid="count: a visible neutral" if vis else None)
    return q


def cooccurrence(fx, F, m, slug):
    gold, neutral = {}, {}
    for o in fx.vocab:
        if o == m:
            continue
        wit, possible, visible = [], [], False
        for k in fx.keys:
            atoms = [F.contains(k, m), F.contains(k, o)]
            v, n = judge(fx, k, atoms)
            if v == "gold":
                wit.append([fact_out(a) for a in atoms])
            elif v == "neutral":
                possible.append(fx.hid(k)); visible |= n["visible"]
        if wit:
            gold[o] = {"id": o, "name": display_mint(o), "match": [fold(x) for x in names_for(o)], "witnesses": wit}
        elif possible:
            neutral[o] = {"id": o, "name": display_mint(o), "match": [fold(x) for x in names_for(o)], "visible": visible,
                          "why": f"co-occurs only in {possible}"}
    return {"id": f"k2-cooc-{slug(m)}", "class": "co-occurrence", "answer_type": "mints",
            "question": f"Which mints are represented in hoards alongside coins of {display_mint(m)}?",
            "gold_items": list(gold.values()), "neutral_items": list(neutral.values()), "params": {"mints": [m]}}


def superlatives(fx, F, V, R, slug):
    out = []

    def best(score):        # unique argmax or None
        if not score:
            return None
        top = max(score.values())
        w = [k for k, v in score.items() if v == top]
        return w[0] if len(w) == 1 and top > 0 else None

    def hoards_with(atoms_of, reading):
        """reading 'gold': dev hoards whose facts are gold; 'text': any hoard whose facts the text allows."""
        res = []
        for k in fx.keys:
            atoms = atoms_of(k)
            if reading == "gold" and fx.role[k] == "dev" and all(a["text"] == "T" and a["truth"] == "T" for a in atoms):
                res.append((k, atoms))
            if reading == "text" and all(a["text"] in ("T", "A") for a in atoms):
                res.append((k, atoms))
        return res

    # S1: the mint in the most hoards (optionally within a region)
    for r in [None] + R:
        def atoms_of(m, r=r):
            return lambda k: ([F.region(k, r)] if r else []) + [F.contains(k, m)]
        g = {m: len(hoards_with(atoms_of(m), "gold")) for m in fx.vocab}
        t = {m: len(hoards_with(atoms_of(m), "text")) for m in fx.vocab}
        w = best(g)
        where = f" found in {REGIONS[r]}" if r else ""
        second = sorted((m for m in g if m != w), key=lambda m: -g[m])[:1]
        needed = [fact_out(a) for m in ([w] + second if w else []) for _, atoms in hoards_with(atoms_of(m), "gold") for a in atoms]
        out.append({"id": f"k2-sup-mint-most-hoards{'-' + slug(r) if r else ''}", "class": "superlative", "answer_type": "mint",
                    "question": f"Which mint is represented in the most hoards{where}?", "form": "mint-most-hoards",
                    "answer": w, "answer_name": display_mint(w) if w else None,
                    "answer_match": [fold(x) for x in names_for(w)] if w else [], "needed_facts": needed,
                    "params": {"region": r}, "readings": {"gold": {display_mint(m): g[m] for m in g if g[m]},
                                                          "text": {display_mint(m): t[m] for m in t if t[m]}},
                    "invalid": ("one candidate" if sum(1 for v in g.values() if v) < 2 or
                                len({k for m in g for k, _ in hoards_with(atoms_of(m), "gold")}) < 2
                                else None if w and best(t) == w else "winner changes in the text reading")})
    # S2: the hoard with the most mints (optionally within a region)
    for r in [None] + R:
        def mints(k, reading, r=r):
            if r:
                a = F.region(k, r)
                if reading == "gold" and not (a["text"] == "T" and a["truth"] == "T") or reading == "text" and a["text"] != "T":
                    return []
            if reading == "gold" and fx.role[k] != "dev":
                return []
            return [m for m in fx.vocab if (lambda a: a["text"] == "T" and (reading == "text" or a["truth"] == "T"))(F.contains(k, m))]
        g = {k: len(mints(k, "gold")) for k in fx.keys}
        t = {k: len(mints(k, "text")) for k in fx.keys}
        w = best(g)
        where = f" found in {REGIONS[r]}" if r else ""
        second = sorted((k for k in g if k != w), key=lambda k: -g[k])[:1]
        needed = [fact_out(F.contains(k, m)) for k in ([w] + second if w else []) for m in mints(k, "gold")]
        needed += [fact_out(F.region(k, r)) for k in fx.keys if r and mints(k, "gold")]
        out.append({"id": f"k2-sup-hoard-most-mints{'-' + slug(r) if r else ''}", "class": "superlative", "answer_type": "hoard",
                    "question": f"Which hoard{where} contains coins from the most mints?", "form": "hoard-most-mints",
                    "answer": fx.hid(w) if w else None, "answer_name": fx.name[w] if w else None,
                    "answer_match": fx.match[w] if w else [], "needed_facts": needed, "params": {"region": r},
                    "readings": {"gold": {fx.name[k]: g[k] for k in g if g[k]}, "text": {fx.name[k]: t[k] for k in t if t[k]}},
                    "invalid": ("one candidate" if sum(1 for v in g.values() if v) < 2 else None if w and best(t) == w
                                else "winner changes in the text reading")})
    # S3: earliest / latest burial among hoards with mint M, or found in region R
    for way, (kind, val) in itertools.product(("earliest", "latest"), [("mint", m) for m in V] + [("region", r) for r in R]):
        def atoms_of(k, kind=kind, val=val):
            return [F.contains(k, val)] if kind == "mint" else [F.region(k, val)]
        def dated(reading):
            res = {}
            for k, atoms in hoards_with(atoms_of, reading):
                if reading == "gold":
                    dep = None if isinstance(k, str) else fx.truth[k]["deposit"]
                    if k in DATE_TEXT and dep:
                        n = int(re.match(r"(\d+)", dep).group(1))
                        res[k] = (n, n)
                elif k in DATE_TEXT:
                    res[k] = DATE_TEXT[k][:2]
            return res
        def strict(d):      # winner whose whole interval beats every rival's whole interval
            for k, (e, l) in d.items():
                rivals = [v for j, v in d.items() if j != k]
                if rivals and all((l > re_ if way == "earliest" else e < rl) for re_, rl in rivals):
                    return k
            return None
        g, t = dated("gold"), dated("text")
        w = strict(g)
        what = f"containing coins of {display_mint(val)}" if kind == "mint" else f"found in {REGIONS[val]}"
        needed = [fact_out(a) for k in g for a in atoms_of(k)] + [F.date(k) for k in g]
        out.append({"id": f"k2-sup-{way}-{slug(val)}", "class": "superlative", "answer_type": "hoard",
                    "question": f"Which hoard {what} was buried {way}?" if kind == "region" else
                                f"Which is the {way}-buried hoard {what}?", "form": f"{way}-{kind}",
                    "answer": fx.hid(w) if w else None, "answer_name": fx.name[w] if w else None,
                    "answer_match": fx.match[w] if w else [], "needed_facts": needed, "params": {kind: val, "way": way},
                    "readings": {"gold": {fx.name[k]: v for k, v in g.items()}, "text": {fx.name[k]: v for k, v in t.items()}},
                    "invalid": ("one candidate" if len(g) < 2 else None if w and strict(t) == w
                                else "winner changes in the text reading")})
    for q in out:
        q["gold_items"], q["neutral_items"] = [], []
        q["open_world_sensitive"] = True
    return out


def uncertainty(fx, F):
    out = []
    for k in fx.keys:
        if fx.role[k] != "dev":
            continue
        unc = [m for m in (fx.truth[k].get("uncertain") or {}).get("mints", []) if m != "Uncertain value"]
        if not unc:
            continue
        gold, examined = [], []
        for m in unc:
            hits = []
            for cid, text, _ in fx.text_of(k):
                for nm in names_for(m):
                    for x in crx(nm).finditer(deaccent(text)):
                        if HEDGE.search(text[max(0, x.start() - HEDGE_WINDOW):x.end() + HEDGE_WINDOW]):
                            hits.append(cid)
            examined.append({"mint": m, "named_in_text": F.contains(k, m)["text"] == "T", "hedged_chunks": hits})
            if hits:
                gold.append({"id": m, "name": display_mint(m), "match": [fold(x) for x in names_for(m)],
                             "witnesses": [[{"fact": f"uncertain {fx.hid(k)} {display_mint(m)}", "chunks": sorted(set(hits))}]]})
        out.append({"id": f"k2-unc-{fx.hid(k)}", "class": "uncertainty", "answer_type": "mints",
                    "question": f"Which mint attributions in the {fx.name[k]} hoard are uncertain?",
                    "gold_items": gold, "neutral_items": [], "params": {"hoard": fx.hid(k)}, "examined": examined})
    return out


# ── selection, spans, output ───────────────────────────────────────────────
def select(cls, cands):
    judged = []
    for q in cands:
        vis = [n for n in q["neutral_items"] if n.get("visible")]
        if q["answer_type"] in ("hoard", "mint"):
            ok = q["answer"] is not None and not q["invalid"]
            why = q["invalid"] or "no strict winner in the gold reading"
            key = (0, 0, 0, q["id"])
            div = q["id"]
        else:
            ok = bool(q["gold_items"]) and len(vis) <= len(q["gold_items"]) and not q.get("invalid")
            why = "empty gold" if not q["gold_items"] else q.get("invalid") or "visible neutrals > gold"
            key = (q.get("template", 0), -(len(q["gold_items"]) - len(vis)), -len(q["gold_items"]), q["id"])
            div = frozenset(g["id"] for g in q["gold_items"])
        judged.append((ok, why, key, div, q))
    kept, seen = [], set()
    for ok, why, key, div, q in sorted(judged, key=lambda x: x[2]):
        if ok and div not in seen and len(kept) < CAP:
            kept.append(q); seen.add(div)
    kept_ids = {q["id"] for q in kept}
    reasons = collections.Counter("kept" if q["id"] in kept_ids else ("repeats a kept answer" if ok and div in seen
                                                                      else "over cap" if ok else why)
                                  for ok, why, key, div, q in judged)
    return kept, dict(reasons)


def span(fx, qs):
    """How widely a class's gold reaches: distinct hoards / sections / chunks its facts cite, and
    the share of all facts held by the single most-cited hoard (a hub check)."""
    c2s = {cid: c["id"] for c in fx.chapters for cid in c["chunk_ids"]}
    by_hoard, secs, chunks = collections.Counter(), set(), set()
    for q in qs:
        for f in [f for g in q["gold_items"] for w in g["witnesses"] for f in w] + q.get("needed_facts", []):
            by_hoard[f["fact"].split()[1]] += 1
            chunks.update(f["chunks"]); secs.update(c2s[c] for c in f["chunks"])
    top = by_hoard.most_common(1)
    return {"questions": len(qs), "distinct_hoards": len(by_hoard), "distinct_sections": len(secs),
            "distinct_chunks": len(chunks), "hoards": sorted(by_hoard),
            "top_hoard": top[0][0] if top else None,
            "top_hoard_share": round(top[0][1] / sum(by_hoard.values()), 2) if top else None}


def toml_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return str(v)
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=False)
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{" + ", ".join(f"{json.dumps(str(k), ensure_ascii=False)} = {toml_value(x)}" for k, x in v.items() if x is not None) + "}"
    raise TypeError(type(v))


def write_toml(fx, bank, spans, census):
    L = ['[bank]', 'name = "ontology-proof-ans-k2-dev"', f'corpus = "{CORPUS}"',
         'description = "K2 semantic queries (joins, filters, counts, superlatives, negation) over the dev hoard fixture; '
         'gold = truth/hoards.json facts the fixture text attests (k2_bank.py). Neutral items are scored as neither found nor made up."',
         f"census = {toml_value(census)}", f"spans = {toml_value(spans)}", ""]
    for k in fx.keys:
        L += ["[[hoards]]", f'id = "{fx.hid(k)}"', f"name = {toml_value(fx.name[k])}", f'role = "{fx.role[k]}"',
              f"match = {toml_value(fx.match[k])}", f"chunks = {toml_value(fx.chunks_of(k))}", ""]
    for label in fx.vocab:                               # every truth label: the T0 resolver reads these
        L += ["[[mints]]", f"id = {toml_value(label)}", f"name = {toml_value(display_mint(label))}",
              f"match = {toml_value([fold(x) for x in names_for(label)])}", ""]
    for q in bank:
        lists = q["answer_type"] in ("hoards", "mints")
        expected = ([g["name"] for g in q["gold_items"]] if lists else [str(q["answer"])] if q["answer_type"] == "int"
                    else [q["answer_name"]])
        notes = (f"{q['class']}; gold {len(q['gold_items'])}" if lists else f"{q['class']}; answer {expected[0]}")
        notes += f"; neutral {len(q.get('neutral_items', []))}"
        notes += "; OPEN_WORLD_SENSITIVE" if q.get("open_world_sensitive") else ""
        implied = sum(1 for f in [f for g in q["gold_items"] for w in g["witnesses"] for f in w] + q.get("needed_facts", [])
                      if f.get("implied"))
        notes += f"; IMPLIED_FACTS {implied}" if implied else ""
        L += ["[[questions]]", f'id = "{q["id"]}"', f'category = "k2_{q["class"].replace("-", "_")}"', f'class = "{q["class"]}"',
              f"question = {toml_value(q['question'])}", f"expected_facts = {toml_value(expected)}",
              f'answer_type = "{q["answer_type"]}"', f"open_world_sensitive = {toml_value(bool(q.get('open_world_sensitive')))}",
              f"implied_facts = {implied}",
              f"params = {toml_value(q.get('params', {}))}", f"notes = {toml_value(notes)}"]
        if lists:
            L.append(f"answer = {toml_value([g['id'] for g in q['gold_items']])}")
        else:
            L.append(f"answer = {toml_value(q['answer'])}")
            if q["answer_type"] != "int":
                L.append(f"answer_match = {toml_value(q['answer_match'])}")
            L.append(f"needed_facts = {toml_value(q['needed_facts'])}")
            if "readings" in q:
                L.append(f"readings = {toml_value({k: {n: list(v) if isinstance(v, tuple) else v for n, v in r.items()} for k, r in q['readings'].items()})}")
        L.append(f"neutral = {toml_value([n['id'] for n in q.get('neutral_items', [])])}")
        for g in q["gold_items"]:
            L += ["[[questions.gold_items]]", f"id = {toml_value(g['id'])}", f"name = {toml_value(g['name'])}",
                  f"match = {toml_value(g['match'])}", f"witnesses = {toml_value(g['witnesses'])}"]
        for n in q.get("neutral_items", []):
            L += ["[[questions.neutral_items]]", f"id = {toml_value(n['id'])}", f"name = {toml_value(n['name'])}",
                  f"match = {toml_value(n['match'])}", f"visible = {toml_value(n['visible'])}", f"why = {toml_value(n['why'])}"]
        L.append("")
    OUT.write_text("\n".join(L), encoding="utf8")


def audit(fx):
    for k in fx.keys:
        print(f"\n## {fx.hid(k)} {fx.name[k]!r} [{fx.role[k]}] chunks {fx.chunks_of(k)}")
        truth = set() if isinstance(k, str) else set(fx.truth[k]["mints"])
        for label, ms in sorted(fx.mentions[k].items()):
            for cid, v, ctx in ms:
                print(f"   {display_mint(label):12} {'truth' if label in truth else '-----'} c{cid:<3} {v:11} …{ctx}…")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--audit", action="store_true")
    a = ap.parse_args()
    fx = Fixture()
    F = Facts(fx)
    if a.audit:
        audit(fx)
    cands, V, R = candidates(fx, F)
    order = ["intersection", "constraint-date", "constraint-region", "count", "superlative", "co-occurrence", "uncertainty", "negation"]
    bank, census, spans = [], {}, {}
    for cls in order:
        kept, reasons = select(cls, cands[cls])
        bank += kept
        census[cls] = {"candidates": len(cands[cls]), "kept": len(kept), "verdicts": reasons}
        spans[cls] = span(fx, kept)
    write_toml(fx, bank, spans, census)
    import tomllib
    back = tomllib.loads(OUT.read_text())
    assert len(back["questions"]) == len(bank)
    roles = collections.Counter(fx.role.values())
    print(f"fixture {CORPUS}: hoards {dict(roles)}; attested vocabulary {len(V)} mints; regions {R}")
    print(f"{'class':18} {'cand':>5} {'kept':>4} {'hoards':>6} {'secs':>4} {'chunks':>6} {'top hoard':>17}  verdicts")
    for cls in order:
        s, c = spans[cls], census[cls]
        top = f"{s['top_hoard']} {s['top_hoard_share']}" if s["top_hoard"] else "-"
        print(f"{cls:18} {c['candidates']:5} {c['kept']:4} {s['distinct_hoards']:6} {s['distinct_sections']:4} "
              f"{s['distinct_chunks']:6} {top:>17}  {c['verdicts']}")
    for q in bank:
        ans = ([g["name"] for g in q["gold_items"]] if q["answer_type"] in ("hoards", "mints") else q["answer_name"]
               if q["answer_type"] != "int" else q["answer"])
        print(f"  {q['id']:42} {q['question']}\n  {'':42} -> {ans}   neutral {[n['id'] for n in q.get('neutral_items', [])]}")
    print(f"-> {OUT.name} ({len(bank)} questions)")


if __name__ == "__main__":
    main()
