#!/usr/bin/env python3
"""Generated deal worlds buried in Kim Ward's real Enron mail, each with an exact answer key.

    python3 build_world.py --seeds 1,2,3,4,5          # one world per seed: ~/.svrnmesh/bench-corpora/world-s<N>/
    python3 build_world.py --seeds 3 --out /tmp/w3    # one world anywhere, for rapid development
    python3 build_world.py --seeds 1,2,3,4,5 --check  # re-validate written worlds and redo heterogeneity.json

Every setting (sizes, rates, the mix of reference forms, noise, hay, timeline span, lifecycle shapes, how many of
each change over time) is drawn per world from a range and recorded in its manifest.json, so a reader cannot learn
one generator's habits instead of reading. Code draws the world: counterparties of five kinds under several naming
patterns, people who each keep one persona, deals from a catalogue, dated stage moves, and changes over time
(renames, contact turnover, trader hand-offs, repricing, volume or term changes, corrections, regressions, stalls,
cancellations and revivals, deals rolled into new ones). MiMo writes each body under a random style card and
temperature and wraps the planned fact in ⟦1| ⟧; code checks it, strips the markers and keeps the marked words as
the gold quote. A body that fails four times is not written and its updates are counted as dropped. The key is
built from what was written: a change whose message was dropped never reaches it. Commitments are the planned ones.

A world: story/, gold/ (GOLD_SPEC shape; a renamed company is one record with both domains and an "aka" list),
tags.jsonl (difficulty tags per gold stage update and commitment), msgids.json, recipe.toml, manifest.json,
bodies.json (the written bodies, for heterogeneity.json beside the worlds). Run one with research/ontology-apps/loop/loop.sh; compare
two runs over several worlds with compare_runs.py.
"""
import argparse, json, random, re, shutil, sys, textwrap, threading, time, urllib.error, urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

HERE = Path(__file__).resolve().parent
RECIPE = HERE.parent / "ward" / "recipe-story.toml"
ROOT = Path.home() / ".svrnmesh/bench-corpora"
WARD = ROOT / "enron-ward"
OPENCODE = Path.home() / ".config/opencode/opencode.json"
DEBUG = __import__("os").environ.get("WORLD_DEBUG")
MODEL = "mimo-v2.6-flash"
LA = ZoneInfo("America/Los_Angeles")
# score_ward.py's stop list: a quote needs two words outside it or the checker can never match it.
STOP = set("a an the and or of to in on for at by with from as is are was were be been it its this that these those we you i he she they them our your my me us if so but not no do does did have has had will would can could should please any all also there here than then just about into over under up out".split())

FIRST = ("Dana Marcus Elaine Trent Gloria Wes Renee Darrell Joan Curtis Leah Hollis Marta Glen Bev Arturo Lorraine Neil "
         "Tamsin Russ Corinne Floyd Ines Boyd Sheila Vern Odette Clint Myra Desmond Priya Lyle Fern Otis Nadine Rollo "
         "Celia Hank Imogen Jules Kareem Lupe Mavis Nils Orla Perry Quinn Rosa Saul Thea Ugo Vida Wade Yusuf Zelda Abe "
         "Bettina Cyrus Delia Edwin Frida Gus Hattie Irv Jolene").split()
SYL1 = "Ash Black Brad Corn Dun Fair Hal Lang Mar Pem Rad Stan Thorn Whit Wyn Hart Kel Mor Pen Ros Sel Tal Ver Wex Yar Quin Bram Cal Dor Fen".split()
SYL2 = "ford ley wick worth more ton by dale croft well son stead holt ridge field brook lund vik".split()
PLACE1 = ("Coyote Pinyon Sage Quartz Tamarack Cholla Juniper Bitter Painted Copper Kestrel Lark Iron Alder Blue Cotton "
          "Elk Raven Osprey Basalt Cinder Willow Badger Antelope Falcon Granite Cedar Sumac Yarrow Heron").split()
PLACE2 = "Basin Ridge Valley Hill Falls Springs Flat Bluff Hollow Point Creek Gap Butte Canyon Lake Prairie Bend Wash".split()
COIN1, COIN2 = "Nor Tal Quan Ard Bel Cor Dyn Ev Lum Ver Zan Kor Pell Tor Sym Vel Ast Bry".split(), "vex ica on ara is um ent ix era ana ova".split()
STATES = "az ca nm nv or ut co".split()
NAMING = {  # kind: name patterns ({P} place, {L} surname, {C} coined) and the deal types it trades
    "muni": (["City of {P}", "{P} Municipal Utilities", "{P} Public Power District", "{P} Water and Power"],
             ["fixed", "index", "call", "power", "master"]),
    "coop": (["{P} Electric Cooperative", "{P} Power Cooperative", "{P} Rural Electric Association"],
             ["fixed", "index", "put", "power", "tolling", "master"]),
    "industrial": (["{L} Industries", "{P} Cement Company", "{P} Paper Mills", "{L} Steel Corporation", "{C} Chemical"],
                   ["fixed", "index", "call", "storage", "master"]),
    "marketer": (["{C} Energy Marketing, LLC", "{C} Resources", "{C} Trading Company"],
                 ["fixed", "index", "call", "put", "transport", "storage", "power", "master"]),
    "pipeline": (["{P} Pipeline Company", "{P} Gas Transmission", "{C} Midstream"], ["transport", "storage", "index", "master"]),
}
CP_ROLES = ["fuels buyer", "gas supply supervisor", "resource planner", "finance director", "staff attorney", "energy manager"]
GAS = ["Malin", "PG&E Citygate", "SoCal Border", "Topock", "San Juan", "Permian", "Kingsgate", "Opal", "Sumas", "Waha"]
GAS_VOL = ["2,500/d", "5,000 MMBtu/d", "~10k/day", "3,000/d", "7,500 a day", "1,000 MMBtu/day", "4k/d", "15,000/d"]
POWER = ["Palo Verde", "Mid-C", "COB", "Four Corners", "Mead"]
TYPES = {  # product, descriptor, nickname, points, volumes, (price low, high, format)
    "fixed": ("fixed-price gas", "fixed-price", "the fixed-price deal", GAS, GAS_VOL, (2.8, 9.5, "${}/MMBtu")),
    "index": ("index gas supply", "index", "the index deal", GAS, GAS_VOL, (0.01, 0.12, "Gas Daily {sign} ${}")),
    "call": ("a call option on gas", "call option", "the call option", GAS, GAS_VOL, (0.10, 0.60, "${}/MMBtu premium")),
    "put": ("a put option on gas", "put option", "the put option", GAS, GAS_VOL, (0.08, 0.50, "${}/MMBtu premium")),
    "transport": ("firm transport capacity", "capacity", "the capacity deal", ["San Juan-Topock", "Permian-Waha", "Opal-Malin",
                  "Kingsgate-Malin"], ["10,000 Dth/d", "25k/d", "5,000 Dth/day"], (0.02, 0.35, "${}/Dth demand")),
    "storage": ("gas storage service", "storage", "the storage deal", GAS, ["500k MMBtu", "1.2 Bcf", "750,000 MMBtu", "2 Bcf"],
                (0.15, 0.90, "${}/MMBtu a season")),
    "power": ("an on-peak power block", "on-peak", "the on-peak block", POWER, ["25 MW", "50 MW", "100 MW"], (28, 260, "${}/MWh")),
    "tolling": ("a tolling agreement on their peaker", "tolling", "the tolling deal", POWER, ["80 MW", "120 MW", "45 MW"],
                (4, 14, "${}/kW-month")),
}
GAS_TYPES = ["fixed", "index", "call", "put"]
SHORT = {"PG&E Citygate": "Citygate", "SoCal Border": "Border"}
MASTERS = {"ISDA": "ISDA master agreement for financial trading", "EEI": "EEI Master Power Purchase and Sale Agreement",
           "GISB": "GISB base contract for gas", "Enfolio": "Enfolio Master Firm Purchase/Sale Agreement"}
SHAPES = {"won": [["lead", "proposal", "won"], ["lead", "proposal", "negotiating", "won"], ["proposal", "negotiating", "won"]],
          "lost": [["lead", "proposal", "lost"], ["proposal", "lost"], ["proposal", "negotiating", "lost"]],
          "open": [["lead", "proposal"], ["lead", "proposal", "negotiating"], ["lead"]],
          "regress": [["lead", "proposal", "negotiating", "proposal", "won"], ["proposal", "negotiating", "proposal", "negotiating"]],
          "stall": [["lead", "proposal", "~", "negotiating", "won"], ["proposal", "~", "negotiating"]],
          "revive": [["lead", "proposal", "lost", "proposal", "won"], ["proposal", "lost", "~", "negotiating"]],
          "cancel_revive": [["proposal", "negotiating", "won", "lost", "negotiating", "won"], ["lead", "proposal", "won", "lost", "won"]]}
MASTER_SHAPES = [["lead", "negotiating", "won"], ["negotiating", "won"], ["negotiating", "negotiating", "won"],
                 ["lead", "negotiating"], ["negotiating", "~", "negotiating", "won"]]
ORDER = {"lead": 0, "proposal": 1, "negotiating": 2, "won": 3, "lost": 3}
DYNAMICS = ["rename", "turnover", "handoff", "renegotiation", "term_change", "correction", "superseded"]
FLAGS = DYNAMICS + ["regress", "stall", "cancel", "revive"]
HAY_FOLDERS = ["tep", "mesa", "pasadena", "smud", "palo_alto", "el_paso_electric", "bhp", "smurfit", "akzo_nobel___filtrol",
               "citizens_utilities"]
PERSONAS = {"terse": ("very terse: fragments, no greeting, no pleasantries", "1-2 lines"),
            "verbose": ("chatty and long-winded; gives background and adds asides", "5-9 lines"),
            "formal": ("formal: complete sentences, a salutation and 'Regards'", "3-6 lines"),
            "sloppy": ("sloppy: lowercase, typos, run-on sentences, little punctuation", "2-5 lines"),
            "initialisms": ("heavy on initialisms and desk jargon (FP, GD, LOI, conf, cp, bidweek)", "1-4 lines"),
            "polite": ("polite and warm, with thanks and small courtesies", "3-6 lines")}
CARDS = ["You write single email bodies for a 2000-2001 natural gas and power marketing desk (Enron North America) and "
         "its customers, the way their real mail read: plain text, shorthand, numbers often without units, relative dates.",
         "You are reconstructing one message from a 2001 corporate mailbox at a Houston energy trader. Business email of "
         "the Lotus Notes and Outlook era: plain text, no formatting, people writing quickly between calls.",
         "Write one plain-text email as it was sent in 2000 or 2001 between an energy trading desk and a utility, "
         "industrial or pipeline customer. Real people in a hurry: half sentences are fine.",
         "Draft the body of a single 2001 business email about wholesale gas or power, internal or customer "
         "correspondence at Enron North America, written by someone with their own habits."]
RULES = (" Wrap exactly the sentences you are asked to mark, as ⟦n|sentence⟧. Mention only the details this writer would; never list every term. Never restate these instructions. Do not "
         "invent other deals, prices, volumes or terms. Output only the body: no subject, no headers, no quoted history, "
         "no signature block. Use only the people and companies named in the request.")
# What a message does, by stage and who writes it (cp: the customer; out: Enron to the customer; internal: Enron to
# Enron). Phrased as an act so the writer performs it rather than reciting a status.
ACT = {
    ("lead", "cp"): "ask Enron for a price on it",
    ("lead", "internal"): "tell a colleague the customer called asking for a price on it",
    ("lead", "out"): "ask the customer whether they want a price on it",
    ("proposal", "out"): "quote the customer a price on it",
    ("negotiating", "cp"): "send back comments on the contract language for it (credit, legal or billing; never price, volume, point or dates)",
    ("negotiating", "out"): "send the customer a contract draft or redlines for it",
    ("negotiating", "internal"): "say what credit or legal still needs before the paper on it can go out (a guaranty, an LC, comments)",
    ("won", "cp"): "accept it: tell Enron to go ahead",
    ("won", "out"): "confirm to the customer that it is done",
    ("won", "internal"): "tell colleagues it is done and to book it",
    ("lost", "cp"): "decline it: you went with another supplier or are not doing it",
    ("lost", "out"): "tell the customer Enron is pulling its offer on it",
    ("lost", "internal"): "tell colleagues the customer passed or went with someone else",
}
MASTER_ACT = {("lead", "cp"): "ask Enron to get it in place", ("lead", "internal"): "tell a colleague the customer wants one in place",
              ("won", "cp"): "say your side signed it and is sending it back", ("won", "out"): "say the signed copies are done",
              ("won", "internal"): "tell colleagues it is signed", ("lost", "cp"): "say you will not sign it",
              ("lost", "internal"): "tell colleagues they will not sign it"}
# Ways to imply a stage; each world draws two per stage, so no phrasing runs through every world.
IMPLIED = {"lead": ["can you get me a number on", "what would it take to", "any appetite to price", "need an indication on",
                    "who can quote me", "we'd look at something for"],
           "proposal": ["a bare number and the term", "a number marked flat or firm", "a number good until a time today",
                        "we can do (number)", "(number) works on our end", "a number subject to credit"],
           "negotiating": ["legal has it", "comments attached", "send me the paperwork", "credit wants a guaranty first",
                           "our redline is back", "still waiting on their markup"],
           "won": ["let's roll it", "we're good at that number", "go ahead and send the conf", "book it", "done at (number)",
                   "you've got a deal"],
           "lost": ["we're going to pass", "went another way", "not this time", "the board said no", "we'll sit this one out",
                    "someone beat you on price"]}
HABITS = ["Open by pointing at the message below.", "Use a relative date.", "Add one routine line unrelated to the deal.",
          "Add a one-line correction of a small slip of your own that does not touch the deal."]
PROMISES = {("lead", "cp"): "send our load data", ("proposal", "enron"): "send a term sheet",
            ("negotiating", "cp"): "get our attorney's comments back", ("negotiating", "enron"): "send the revised draft",
            ("won", "enron"): "send the confirm", ("won", "cp"): "sign and fax back the confirm", ("lead", "enron"): "work up a price"}
MARK = re.compile(r"⟦([12C])\|(.*?)⟧", re.S)
NUM = re.compile(r"\$?~?\d[\d,]*\.\d+|\$?\.\d+|\d{1,3}(?:,\d{3})+|\d{3,}|\d+k\b")   # prices, volumes, amounts: not "3 yr"
DUE = re.compile(r"\b(today|tonight|tomorrow|same day|this (?:morning|afternoon|week)|next week|(?:by )?(?:the )?end of "
                 r"(?:the )?(?:day|week|month)|(?:by )?(?:monday|tuesday|wednesday|thursday|friday)|asap|shortly)\b", re.I)


@dataclass(eq=False)
class Person:
    name: str
    email: str
    company: str
    role: str
    persona: str
    email2: str = None              # the address after the company's rename
    switch: datetime = None
    start: datetime = None          # turnover: a successor writes from here
    end: datetime = None            # turnover: a leaver writes until here
    replaces: object = None
    announced: bool = False
    first = property(lambda s: s.name.split()[0])
    last = property(lambda s: s.name.split()[-1])
    enron = property(lambda s: s.company == "enron")

    def addr(self, when):
        return self.email2 if self.switch and when >= self.switch else self.email

    def xfrom(self, when):
        return self.name if self.enron else f'"{self.last}, {self.first}" <{self.addr(when)}>'

    def active(self, when):
        return (self.start is None or when >= self.start) and (self.end is None or when < self.end)


@dataclass(eq=False)
class Company:
    id: str
    kind: str
    name: str
    alias: str
    domain: str
    stem: str
    contacts: list = field(default_factory=list)
    trader: Person = None
    disclaimer: bool = False
    quote_gt: bool = False
    renamed: tuple = None           # (when, name, alias, stem, domain)
    announced: bool = False

    def at(self, when):
        r = self.renamed
        return (r[1], r[2], r[3], r[4]) if r and when >= r[0] else (self.name, self.alias, self.stem, self.domain)

    def names(self):
        ids = [(self.name, self.alias, self.stem, self.domain)] + ([self.renamed[1:]] if self.renamed else [])
        return [x for n, a, s, d in ids for x in (n, a, s, d, dbase(d))] + \
               [x for p in self.contacts for x in (p.name, p.last, p.email, p.email2) if x]


@dataclass(eq=False)
class Deal:
    id: str
    cp: Company
    kind: str                       # transaction | master_agreement
    type: str                       # a TYPES key, or the master type
    v: dict                         # product, point, term, term_short, volume, price, price_surface, as first planned
    descriptor: str                 # the words that tell it from its siblings
    nickname: str = None
    dim: str = "none"               # what tells it from its siblings: term | point | product
    term_start: datetime = None
    amount: int = None
    amount_surface: str = None
    events: list = field(default_factory=list)
    handoff: tuple = None           # (when, new trader)
    supersedes: object = None
    cur: dict = None                # the values the written mail has reached
    introduced: bool = False
    told: bool = False              # the new trader has said so
    nick_at: int = None
    n_msgs: int = 0
    last_stage: str = None
    files: list = field(default_factory=list)

    def trader(self, when):
        return self.handoff[1] if self.handoff and when >= self.handoff[0] else self.cp.trader

    def forms(self):
        return [f for f in (self.descriptor, self.nickname and self.nickname[4:]) if f]

    def desc(self, v=None):
        if self.kind == "master_agreement":
            return f"{MASTERS[self.type]} with {self.cp.alias}"
        v = v or self.cur or self.v
        s = f"{v['product'][0].upper()}{v['product'][1:]}, {v['term']}, {v['volume']} {'on' if self.type == 'transport' else 'at'} {v['point']}"
        return s + (f"; rolls over {self.supersedes.id}" if self.supersedes else "")


@dataclass(eq=False)
class Fact:
    deal: Deal
    ev: dict
    quote: str = None
    tags: dict = None
    stage = property(lambda s: s.ev["stage"])


@dataclass(eq=False)
class Msg:
    cp: Company
    date: datetime
    kind: str                       # fact | chatter | nomination
    facts: list = field(default_factory=list)
    thread: object = None
    direction: str = None           # cp | out | internal
    sender: Person = None
    to: list = field(default_factory=list)
    cc: list = field(default_factory=list)
    subject: str = ""
    mode: str = "new"               # new | reply | forward
    prev: object = None
    clean: str = ""
    own: str = ""
    full: str = ""
    msgid: str = ""
    folder: str = ""
    file: str = ""
    deals: set = field(default_factory=set)
    commitment: dict = None
    about: Deal = None              # chatter / nomination subject


class Budget:
    def __init__(self, cap):
        self.cap, self.calls, self.prompt, self.completion, self.lock = cap, 0, 0, 0, threading.Lock()


def norm(s):
    return re.sub(r"\s+", " ", s or "").strip()


def words(s):
    return {w for w in re.findall(r"[a-z0-9]+", (s or "").lower()) if w not in STOP and len(w) > 1}


def dbase(domain):
    return max(domain.split("."), key=len)


def pick(rng, weights):
    return rng.choices(list(weights), list(weights.values()))[0]


def ascii_clean(s):
    for a, b in (("‘", "'"), ("’", "'"), ("“", '"'), ("”", '"'), ("–", "-"),
                 ("—", "-"), ("…", "..."), (" ", " ")):
        s = s.replace(a, b)
    return "".join(ch for ch in s if ord(ch) < 128 or ch in "⟦⟧")


def has(text, form):
    return re.search(r"(?<![a-z0-9])" + re.escape(form.lower()) + r"(?![a-z0-9])", text.lower()) is not None


def misspell(rng, s):
    w = max(s.split(), key=len)
    if len(w) < 5:
        return s + s[-1]
    i = rng.randint(1, len(w) - 3)
    w2 = w[:i] + w[i + 1] + w[i] + w[i + 2:]
    return s.replace(w, w2 if w2 != w else w[:i] + w[i + 1:], 1)


def reprice(rng, s):
    x = float(s)
    step = max(0.01 if "." in s else 1, x * rng.uniform(0.04, 0.18)) * rng.choice([-1, 1])
    y = x + step if x + step > 0 else x - step
    out = f"{y:.2f}".lstrip("0") if "." in s else str(round(y))
    return out if out != s else reprice(rng, s)


# ---------------------------------------------------------------- the settings, the hay and the world

def draw_settings(rng):
    u = lambda a, b: round(rng.uniform(a, b), 2)
    mix = lambda keys: {k: u(0.1, 1.0) for k in keys}
    span = rng.randint(3, 18)
    lo, hi = datetime(2000, 1, 3), datetime(2001, 12, 20) - timedelta(days=30 * span)
    return {"counterparties": rng.randint(6, 12), "max_deals_per_counterparty": rng.randint(2, 4),
            "master_rate": u(0.1, 0.5), "nickname_rate": u(0.15, 0.7),
            "deal_ref": mix(["canonical", "nickname", "implicit"]),
            "counterparty_ref": mix(["canonical", "alias", "domain_only", "misspelt", "implicit"]),
            "forward_rate": u(0.05, 0.35), "reply_quote_gt_rate": u(0.0, 0.7), "internal_rate": u(0.1, 0.45),
            "passing_mention_rate": u(0.05, 0.45), "distractor_rate": u(0.2, 0.9), "merge_rate": u(0.1, 0.6),
            "subject_only_rate": u(0.0, 0.15), "signature_rate": u(0.2, 0.9), "disclaimer_rate": u(0.0, 0.5),
            "typo_rate": u(0.0, 0.5), "attachment_rate": u(0.05, 0.4), "commitment_rate": u(0.1, 0.4),
            "temperature": [u(0.4, 0.7), u(0.8, 1.15)], "hay_folders": rng.sample(HAY_FOLDERS, rng.randint(3, 4)),
            "hay_per_folder": rng.randint(8, 40), "in_real_folders": rng.randint(2, 6), "span_months": span,
            "start": (lo + timedelta(days=rng.randint(0, (hi - lo).days))).strftime("%Y-%m-%d"),
            "lifecycle": mix(list(SHAPES)), "dynamics": {k: rng.randint(1, 2) for k in DYNAMICS},
            "implied": {k: rng.sample(v, 2) for k, v in IMPLIED.items()}}


def load_hay(folders, per):
    hay = []
    for f in folders:
        d = WARD / "all" / f
        names = sorted((p.name for p in d.iterdir() if p.is_file()), key=lambda n: int(n.rstrip(".")))[:per]
        hay += [(f, n, (d / n).read_text(errors="replace")) for n in names]
    return hay


def make_world(rng, S, hay_lower, used):
    """Companies, people, deals and their changes over time; every name is new to the hay and to the other worlds."""
    def free(*ns):
        return all(n.lower() not in hay_lower and n.lower() not in used for n in ns if n)
    firsts = {"Kim"}                                  # one first name per person within a world: sign-offs stay clear

    def person(cid, domain, role):
        for _ in range(20000):
            first, last = rng.choice(FIRST), rng.choice(SYL1) + rng.choice(SYL2)
            local = f"{first}.{last}" if domain == "enron.com" else rng.choice([f"{first}.{last}", f"{first[0]}{last}", f"{first}{last[0]}"])
            email = f"{local.lower()}@{domain}"
            if first not in firsts and free(f"{first} {last}", last, email):
                used.update(x.lower() for x in (f"{first} {last}", last, email))
                firsts.add(first)
                return Person(f"{first} {last}", email, cid, role, rng.choice(list(PERSONAS)))
        sys.exit("ran out of person names")

    def identity(kind):
        for _ in range(20000):
            pat = rng.choice(NAMING[kind][0])
            P = (f"{rng.choice(PLACE1)} {rng.choice(PLACE2)}" if rng.random() < 0.6
                 else rng.choice(PLACE1) + rng.choice(["ton", "ville", "dale", "field", "wood"]))
            L, C = rng.choice(SYL1) + rng.choice(SYL2), rng.choice(COIN1) + rng.choice(COIN2)
            name = pat.format(P=P, L=L, C=C)
            stem = P if "{P}" in pat else L if "{L}" in pat else C
            initials = "".join(w[0] for w in name.replace(",", "").split() if w not in ("of", "and", "LLC", "City")).upper()
            alias = initials if len(initials) >= 3 and rng.random() < 0.5 else stem
            flat = stem.lower().replace(" ", "")
            domain = (f"ci.{flat}.{rng.choice(STATES)}.us" if name.startswith("City of") else
                      rng.choice([flat] + ([initials.lower()] if len(initials) >= 3 else [])) + rng.choice([".com", ".com", ".org", ".net"]))
            cid = stem.lower().replace(" ", "_")
            if cid not in HAY_FOLDERS and free(name, alias, stem, domain, dbase(domain), cid):
                used.update(x.lower() for x in (name, alias, stem, domain, dbase(domain), cid))
                return name, alias, stem, domain, cid
        sys.exit("ran out of company names")

    staff = {"traders": [person("enron", "enron.com", "gas originator") for _ in range(rng.randint(3, 5))],
             "credit": [person("enron", "enron.com", "credit analyst") for _ in range(rng.randint(1, 2))],
             "legal": [person("enron", "enron.com", "senior counsel") for _ in range(rng.randint(1, 2))],
             "kim": Person("Kim Ward", "kim.ward@enron.com", "enron", "gas originator", "terse")}
    w0 = datetime.fromisoformat(S["start"]).replace(tzinfo=LA)
    w1 = w0 + timedelta(days=30 * S["span_months"])
    cps, deals = [], []
    for _ in range(S["counterparties"]):
        kind = rng.choice(list(NAMING))
        name, alias, stem, domain, cid = identity(kind)
        c = Company(cid, kind, name, alias, domain, stem, disclaimer=rng.random() < S["disclaimer_rate"],
                    quote_gt=rng.random() < S["reply_quote_gt_rate"])
        c.contacts = [person(cid, domain, r) for r in rng.sample(CP_ROLES, rng.randint(1, 3))]
        c.trader = rng.choice(staff["traders"])
        cps.append(c)
        deals += make_deals(rng, S, c, w0, w1)
    dyn = add_dynamics(rng, S, cps, deals, staff, identity, person, w1)
    for c in cps:
        for i, d in enumerate(sorted((d for d in deals if d.cp is c), key=lambda d: d.events[0]["date"]), 1):
            d.id, d.cur = f"{c.id}-d{i}", dict(d.v)
    return cps, staff, deals, dyn


def term_options(d0):
    y = d0.year
    wy = y if d0.month < 11 else y + 1
    sy = y if d0.month < 4 else y + 1
    q = (d0.month - 1) // 3 + 2
    qy = y + (q > 4)
    q = q - 4 if q > 4 else q
    first = (d0.replace(day=1) + timedelta(days=32)).replace(day=1)
    return [("Nov-Mar", f"Nov {wy} - Mar {wy + 1}", "the winter piece", d0.replace(year=wy, month=11, day=1)),
            ("Apr-Oct", f"Apr - Oct {sy}", "the summer piece", d0.replace(year=sy, month=4, day=1)),
            (f"Q{q}", f"Q{q} {qy}", f"the Q{q} deal", d0.replace(year=qy, month=3 * q - 2, day=1)),
            (f"Cal {(y + 1) % 100:02d}", f"Jan - Dec {y + 1}", f"the Cal {(y + 1) % 100:02d} strip", d0.replace(year=y + 1, month=1, day=1)),
            ("2-yr", f"2 years from {first:%b %Y}", "the term deal", first)]


def lifecycle(rng, start, path, end):
    """Dated events for a stage path; '~' is a stall (a long silence). Squeezed to end inside the world's window."""
    offs, t, stall = [], 0.0, False
    for s in path:
        if s == "~":
            t, stall = t + rng.uniform(40, 100), True
            continue
        offs.append((s, t, stall))
        t, stall = t + rng.uniform(2, 21), False
    k = min(1.0, max(1.0, (end - start).days) / max(1.0, offs[-1][1]))
    events, prev, last = [], None, None
    for s, off, st in offs:
        d = start + timedelta(days=off * k)
        while d.weekday() >= 5:
            d += timedelta(days=1)
        d = d.replace(hour=rng.randint(6, 17), minute=rng.choice([0, 5, 12, 20, 31, 40, 47, 55]))
        d = d if not prev or d > prev else prev + timedelta(hours=rng.randint(2, 6))
        note = ("they had walked away from it before" if last == "lost" and s != "lost" else
                "it had been agreed, and now it is off" if last == "won" and s == "lost" else
                "the last round on it fell through" if last and s != "lost" and ORDER[s] < ORDER[last] else None)
        note = note and f"Background, not to be repeated: {note}."
        events.append({"date": d, "stage": s, "dyn": {"stall"} if st else set(), "note": note, "set": {}, "over": {}})
        prev, last = d, s
    return events


def make_deals(rng, S, c, w0, w1):
    kinds = NAMING[c.kind][1]
    tx = [t for t in kinds if t != "master"]
    n = rng.randint(1, S["max_deals_per_counterparty"])
    master = n >= 2 and "master" in kinds and rng.random() < S["master_rate"]
    start = w0 + timedelta(days=rng.uniform(0, 0.5 * (w1 - w0).days))
    terms, base = term_options(start), rng.choice(tx)
    gas = [t for t in tx if t in GAS_TYPES]
    dim = rng.choice(["term", "point"] + (["product"] if base in GAS_TYPES and len(gas) >= 2 else []))
    pool = {"term": terms, "point": TYPES[base][3], "product": gas}[dim]
    vals = rng.sample(pool, min(n - master, len(pool)))
    term0, point0 = rng.choice(terms), rng.choice(TYPES[base][3])
    deals = [Deal("", c, "master_agreement", mt, {}, mt) for mt in ([rng.choice(list(MASTERS))] if master else [])]
    for val in vals:
        typ = val if dim == "product" else base
        T = TYPES[typ]
        term = val if dim == "term" else term0
        point = val if dim == "point" else point0
        lo, hi, fmt = T[5]
        x = rng.uniform(lo, hi)
        ps = f"{x:.2f}".lstrip("0") if hi < 20 else str(round(x))
        v = dict(product=T[0], point=point, term=term[1], term_short=term[0], volume=rng.choice(T[4]), price_surface=ps,
                 price=fmt.format(ps, sign=rng.choice(["plus", "minus"])))
        one = dim if len(vals) > 1 else rng.choice(["term", "point"])
        short = SHORT.get(point, point)
        descriptor, nick = {"term": term[0:3:2], "point": (short, f"the {short} piece"), "product": (T[1], T[2])}[one]
        d = Deal("", c, "transaction", typ, v, descriptor, nickname=nick if rng.random() < S["nickname_rate"] else None,
                 dim=one, term_start=term[3])
        if rng.random() < 0.2:
            mm = rng.choice([0.4, 0.85, 1.2, 2.6, 3.1])
            d.amount, d.amount_surface = int(mm * 1e6), (f"${mm}MM" if mm >= 1 else f"${int(mm * 1000)}k")
        deals.append(d)
    for d in deals:
        shape = (rng.choice(MASTER_SHAPES) if d.kind == "master_agreement" else rng.choice(SHAPES[pick(rng, S["lifecycle"])]))
        d.events = lifecycle(rng, start + timedelta(days=rng.uniform(-10, 30)), shape, w1)
    return deals


def add_dynamics(rng, S, cps, deals, staff, identity, person, end):
    """Changes over time. Each is planned as events; the key takes whatever of them the written mail carries."""
    log = []
    dates = lambda c: sorted(e["date"] for d in deals if d.cp is c for e in d.events)
    tx = lambda: [d for d in deals if d.kind == "transaction"]

    def value(d, i, key):                       # the planned value as of event i
        return next((e["set"][key] for e in reversed(d.events[:i + 1]) if key in e["set"]), d.v[key])

    def between(d, i):
        a = d.events[i]["date"]
        b = d.events[i + 1]["date"] if i + 1 < len(d.events) else a + timedelta(days=rng.uniform(4, 15))
        return a + (b - a) * rng.uniform(0.3, 0.7)

    def insert(d, i, kind, note, when, **kw):
        d.events.insert(i, {"date": when, "stage": d.events[i - 1]["stage"], "dyn": {kind}, "note": note, "set": {}, "over": {}, **kw})

    for kind, n in S["dynamics"].items():
        for _ in range(n):
            if kind == "rename":
                cands = [c for c in cps if not c.renamed and len(dates(c)) >= 2]
                if cands:
                    c = rng.choice(cands)
                    when = dates(c)[len(dates(c)) // 2] - timedelta(hours=1)
                    name, alias, stem, domain, _ = identity(c.kind)
                    c.renamed = (when, name, alias, stem, domain)
                    for p in c.contacts:
                        p.email2, p.switch = p.email.split("@")[0] + "@" + domain, when
                    log.append({"kind": kind, "company": c.id, "date": when, "from": c.name, "to": name})
            elif kind == "turnover":
                cands = [c for c in cps if len(dates(c)) >= 2 and not any(p.start for p in c.contacts)]
                if cands:
                    c = rng.choice(cands)
                    when = dates(c)[rng.randint(1, len(dates(c)) - 1)] - timedelta(hours=1)
                    old = rng.choice(c.contacts)
                    new = person(c.id, c.at(when)[3], old.role)
                    if c.renamed and when < c.renamed[0]:
                        new.email2, new.switch = new.email.split("@")[0] + "@" + c.renamed[4], c.renamed[0]
                    old.end, new.start, new.replaces = when, when, old
                    c.contacts.append(new)
                    log.append({"kind": kind, "company": c.id, "date": when, "left": old.name, "successor": new.name})
            elif kind == "handoff":
                cands = [d for d in deals if len(d.events) >= 2 and not d.handoff]
                if cands:
                    d = rng.choice(cands)
                    when = d.events[rng.randint(1, len(d.events) - 1)]["date"] - timedelta(hours=2)
                    d.handoff = (when, rng.choice([t for t in staff["traders"] if t is not d.cp.trader]))
                    log.append({"kind": kind, "deal": d, "date": when, "from": d.cp.trader.name, "to": d.handoff[1].name})
            elif kind in ("renegotiation", "term_change"):
                ok = ("proposal", "negotiating") if kind == "renegotiation" else ("proposal", "negotiating", "won")
                cands = [(d, i) for d in tx() for i, e in enumerate(d.events[:-1] if kind == "renegotiation" else d.events)
                         if e["stage"] in ok and not e["over"] and "correction" not in e["dyn"]]
                if cands:
                    d, i = rng.choice(cands)
                    if kind == "renegotiation":
                        key = "price_surface"
                    else:
                        key = "term_short" if d.dim != "term" and rng.random() < 0.5 else "volume"
                    old = value(d, i, key)
                    if key == "price_surface":
                        new = reprice(rng, old)
                        sets = {key: new, "price": value(d, i, "price").replace(old, new)}
                    elif key == "volume":
                        new = rng.choice([x for x in TYPES[d.type][4] if x != old])
                        sets = {key: new}
                    else:
                        taken = {x.v["term_short"] for x in deals if x.cp is d.cp and x.kind == "transaction"}
                        opts = [t for t in term_options(d.events[0]["date"]) if t[0] not in taken]
                        if not opts:
                            continue
                        t = rng.choice(opts)
                        new, sets = t[0], {"term_short": t[0], "term": t[1]}
                    what = {"price_surface": "price", "volume": "volume", "term_short": "term"}[key]
                    insert(d, i + 1, kind, f"the {what} changes from {old} to {new}", between(d, i), set=sets)
                    log.append({"kind": kind, "deal": d, "what": what, "from": old, "to": new})
            elif kind == "correction":
                cands = [(d, i) for d in tx() for i, e in enumerate(d.events)
                         if e["stage"] in ("proposal", "negotiating", "won") and not e["over"] and not e["dyn"] & {"correction", "renegotiation", "term_change"}]
                if cands:
                    d, i = rng.choice(cands)
                    e = d.events[i]
                    key = "price_surface" if e["stage"] == "proposal" else "volume"
                    right = value(d, i, key)
                    wrong = reprice(rng, right) if key == "price_surface" else rng.choice([x for x in TYPES[d.type][4] if x != right])
                    e["over"] = {key: wrong}
                    nxt = d.events[i + 1]["date"] if i + 1 < len(d.events) else e["date"] + timedelta(days=2)
                    when = e["date"] + min(timedelta(hours=rng.uniform(1, 5)), (nxt - e["date"]) / 2)
                    insert(d, i + 1, kind, f"correct your last message, briefly, the way people fix a slip: it is {right}, not {wrong}", when, fix=e)
                    log.append({"kind": kind, "deal": d, "what": key.split("_")[0], "wrong": wrong, "right": right})
            elif kind == "superseded":
                cands = [d for d in tx() if d.events[-1]["stage"] != "lost" and not d.supersedes
                         and not any(x.supersedes is d for x in deals)]
                if cands:
                    a = rng.choice(cands)
                    when = a.events[-1]["date"] + timedelta(days=rng.uniform(5, 25))
                    taken = {x.v["term_short"] for x in deals if x.cp is a.cp and x.kind == "transaction"}
                    opts = [t for t in term_options(when) if t[0] not in taken]
                    if not opts:
                        continue
                    t = rng.choice(opts)
                    v = {k: value(a, len(a.events) - 1, k) for k in a.v} | {"term": t[1], "term_short": t[0]}
                    b = Deal("", a.cp, "transaction", a.type, v, t[0], nickname=t[2] if rng.random() < S["nickname_rate"] else None,
                             dim="term", term_start=t[3], supersedes=a)
                    b.events = lifecycle(rng, when, rng.choice([["proposal", "won"], ["proposal", "negotiating", "won"],
                                                               ["negotiating", "won"], ["proposal"], ["proposal", "lost"]]), max(end, when + timedelta(days=45)))
                    first = b.events[0]
                    first["dyn"].add(kind)
                    first["note"] = f"it replaces their earlier {a.v['term_short']} deal"
                    a.events.append({"date": first["date"], "stage": "lost", "dyn": {kind}, "set": {}, "over": {},
                                     "note": f"it is being rolled into a new {t[0]} deal", "merge": (b, first)})
                    deals.append(b)
                    log.append({"kind": kind, "deal": a, "by": b})
    return log


# ---------------------------------------------------------------- the message plan

def plan_messages(rng, S, c, deals):
    """Every message for one counterparty in date order: stage moves (some two to a message), chatter, nominations."""
    evs = sorted(((e, d) for d in deals for e in d.events), key=lambda p: p[0]["date"])
    msgs, taken = [], set()
    for e, d in evs:                                  # a deal rolled into a new one: both in one message
        if e.get("merge"):
            b, e2 = e["merge"]
            msgs.append(Msg(c, max(e["date"], e2["date"]), "fact", facts=[Fact(d, e), Fact(b, e2)]))
            taken |= {id(e), id(e2)}
    if len(deals) >= 2 and rng.random() < S["merge_rate"]:
        ok = [(e, d) for e, d in evs if id(e) not in taken and e is not d.events[0] and not e.get("fix") and not e["over"]]
        for a, da in ok:
            b = next(((e, d) for e, d in ok if d is not da and abs((e["date"] - a["date"]).days) <= 12), None)
            if b:
                msgs.append(Msg(c, max(a["date"], b[0]["date"]), "fact", facts=[Fact(da, a), Fact(b[1], b[0])]))
                taken |= {id(a), id(b[0])}
                break
    for d in deals:
        rest = [e for e in d.events if id(e) not in taken]
        cut = rng.randint(1, len(rest) - 1) if len(rest) >= 3 and rng.random() < 0.3 else len(rest)
        while cut < len(rest) and rest[cut].get("fix"):
            cut += 1                                  # a correction stays with the slip it fixes
        ths = [{"deal": d, "msgs": [], "subject": None} for _ in range(2)]
        msgs += [Msg(c, e["date"], "fact", facts=[Fact(d, e)], thread=ths[j >= cut]) for j, e in enumerate(rest)]
    if rng.random() < S["distractor_rate"]:
        d = rng.choice(deals)
        msgs.append(Msg(c, d.events[0]["date"] + timedelta(days=1, hours=2), "chatter", about=d))
    for d in deals:
        if d.kind == "transaction" and d.events[-1]["stage"] == "won" and rng.random() < S["distractor_rate"]:
            when = max(d.events[-1]["date"] + timedelta(days=rng.randint(6, 20)), d.term_start - timedelta(days=rng.randint(4, 9)))
            msgs.append(Msg(c, when, "nomination", about=d))   # a few days before a month inside the term
    msgs.sort(key=lambda m: m.date)
    return msgs


def choose_direction(rng, S, stage):
    if stage == "proposal":
        return "out"
    if rng.random() < S["internal_rate"]:
        return "internal"
    return {"lead": "cp", "negotiating": rng.choice(["cp", "out"]), "won": "cp" if rng.random() < 0.65 else "out",
            "lost": "cp" if rng.random() < 0.75 else "out"}[stage]


def fill_roles(rng, m, staff):
    c, kim = m.cp, staff["kim"]
    deal = m.facts[0].deal if m.facts else m.about
    trader = deal.trader(m.date)
    active = [p for p in c.contacts if p.active(m.date)] or c.contacts
    known = [p for p in ([m.prev.sender] + m.prev.to if m.prev else []) if p in active]
    contact = known[0] if known and rng.random() < 0.8 else rng.choice(active)
    credit, legal = rng.choice(staff["credit"]), rng.choice(staff["legal"])
    stage = m.facts[0].stage if m.facts else None
    if m.direction == "cp":
        m.sender, m.to, m.cc = contact, [trader], [kim]
    elif m.direction == "out":
        master = any(f.deal.kind == "master_agreement" for f in m.facts)
        m.sender = legal if master and stage == "negotiating" and rng.random() < 0.6 else trader
        m.to, m.cc = [contact], [kim] + ([trader] if m.sender is not trader else [])
    else:
        m.sender = rng.choice([credit, legal, trader]) if stage == "negotiating" else trader
        m.to, m.cc = [rng.choice([p for p in (trader, credit, legal) if p is not m.sender])], [kim]
        if rng.random() < 0.4:
            m.to, m.cc = [kim], m.to


def subject_for(rng, d, when):
    alias = d.cp.at(when)[1]
    if d.kind == "master_agreement":
        return rng.choice([d.type, f"{alias} {d.type}", f"{d.type} draft", "master agreement", d.cp.at(when)[0]])
    return rng.choice([f"{alias} gas", f"{d.descriptor} pricing", d.v["point"], "indication", f"{alias} - {d.descriptor}", "pricing", ""])


def base_subject(s):
    return re.sub(r"^((re|fw|fwd)\s*:\s*)+", "", s, flags=re.I)


# ---------------------------------------------------------------- the prompt and the check

def fmt_people(ps):
    return ", ".join(f"{p.name} ({'Enron' if p.enron else 'customer'}, {p.role})" for p in ps) or "-"


def deal_fields(rng, d, val):
    if d.kind == "master_agreement":
        return f"a {d.type} master agreement"
    term = re.sub(r"\b(19|20)(\d\d)\b", r"\2", val["term"])
    parts = [f"product {val['product']}", f"term {term}", f"volume {val['volume']}",
             f"{'path' if d.type == 'transport' else 'delivery'} {val['point']}"]
    rng.shuffle(parts)
    return "; ".join(parts)


def allowed_numbers(texts, when):
    t = " ".join(x for x in texts if x) + when.strftime(" %Y %m %d ")
    return {numkey(n) for n in NUM.findall(t)} | {"2000", "2001", "2002", "100"}


def numkey(n):
    n = n.replace(",", "").lstrip("$~")
    return n[1:] if n.startswith("0.") else n


def build_spec(rng, S, m, others, cps):
    """What one MiMo call must produce, and what its check demands."""
    c = m.cp
    name, alias, stem, _ = c.at(m.date)
    spec = {"facts": [], "blocks": [], "notes": [], "told": [], "foreign": [n for o in cps if o is not c for n in o.names()],
            "numbers": set(), "nick_in_body": [], "mention": [], "distractor": None, "promise": None, "subject_only": False}
    for k, f in enumerate(m.facts, 1):
        d, ev = f.deal, f.ev
        val = {**d.cur, **ev["set"], **ev["over"]}
        act = (MASTER_ACT.get((f.stage, m.direction)) if d.kind == "master_agreement" else None) or ACT[(f.stage, m.direction)]
        if "superseded" in ev["dyn"] and f.stage == "lost":
            act = "say it is being replaced by a new deal"
        if ev.get("fix"):
            act = ev["note"]
        mine = d.desc(val) + " " + " ".join(d.forms())
        partners = {g.deal for g in m.facts} if "superseded" in ev["dyn"] else {d}
        forbid = [x for o in others if o not in partners for x in o.forms() if not has(mine, x)]
        dw = {k_: w for k_, w in S["deal_ref"].items() if k_ != "nickname" or d.nickname}
        if m.prev is None or len(m.facts) > 1:
            dw.pop("implicit")
        deal_ref = "intro" if not d.introduced else pick(rng, dw)
        cw = S["counterparty_ref"]
        if m.direction == "internal" and m.prev is None:
            cw = {k_: cw[k_] for k_ in ("canonical", "alias", "misspelt")}
        cp_ref = pick(rng, cw)
        lines, req = [], []
        if ev["note"] and not ev.get("fix"):
            lines.append(ev["note"] if ev["note"].startswith("Background") else f"Context: {ev['note']}.")
        if deal_ref == "intro":
            lines.append(f'In that sentence describe the deal by its terms and use the exact words "{d.descriptor}".')
            req.append(d.descriptor)
            if d.nickname:
                lines.append(f'Somewhere in the email call it "{d.nickname}" from now on.')
                spec["nick_in_body"].append(d.nickname[4:])
        elif deal_ref == "nickname":
            lines.append(f'In that sentence call the deal "{d.nickname}" (exact words).')
            req.append(d.nickname[4:])
        elif deal_ref == "canonical":
            lines.append(f'In that sentence refer to the deal with the exact words "{d.descriptor}".')
            req.append(d.descriptor)
        else:
            lines.append("Do not name or describe the deal or its terms; the thread makes it clear ('it', 'that', 'below').")
        if cp_ref in ("canonical", "alias", "misspelt"):
            form = {"canonical": name, "alias": alias, "misspelt": misspell(rng, stem)}[cp_ref]
            lines.append(f'In that sentence name the customer as "{form}" (spelt exactly so).')
            req.append(form)
        else:
            lines.append("Do not name the customer company.")
        ps = val.get("price_surface")
        if ps and (f.stage == "proposal" or "price_surface" in ev["set"] or "price_surface" in ev["over"]
                   or (ev.get("fix") and "price_surface" in ev["fix"]["over"]) or (f.stage == "won" and rng.random() < 0.4)):
            lines.append(f'Include the price as "{ps}" with no unit.')
            req.append(ps)
        for key in ("volume", "term_short"):
            if key in ev["set"] or key in ev["over"] or (ev.get("fix") and key in ev["fix"]["over"]):
                lines.append(f'Include the {key.split("_")[0]} as "{val[key]}".')
                req.append(val[key])
        if d.amount_surface and f.stage == "proposal":
            lines.append(f'Include the total value as "{d.amount_surface}".')
            req.append(d.amount_surface)
        lines.append("Say it plainly." if rng.random() < 0.4 else
                     "Say it only by implication, the way " + " or ".join(f"'{x}'" for x in S["implied"][f.stage]) + " would.")
        rng.shuffle(lines)
        head = (f"Deal {k}: the one in the message below." if deal_ref == "implicit" else
                f"Deal {k} (with {name}): {deal_fields(rng, d, val)}.")
        spec["blocks"].append([head, f"In ONE sentence wrapped as ⟦{k}|...⟧, {act}."] + lines)
        spec["numbers"] |= allowed_numbers([d.desc(d.cur), d.desc(val), ps, d.cur.get("price_surface"), d.amount_surface,
                                            ev["note"]], m.date)
        spec["facts"].append({"fact": f, "required": req, "forbidden": forbid})
        f.cp_ref, f.deal_ref = cp_ref, deal_ref
    rng.shuffle(spec["blocks"])
    d = m.facts[0].deal if m.facts else m.about
    if d.handoff and m.sender is d.handoff[1] and m.date >= d.handoff[0] and not d.told:
        spec["notes"].append(f"Mention you have taken this account over from {d.cp.trader.first}.")
        spec["told"].append((d, "told"))
        spec["mention"].append(d.cp.trader.first)
    if m.sender.replaces and not m.sender.announced:
        spec["notes"].append(f"Mention you have taken over from {m.sender.replaces.name}.")
        spec["told"].append((m.sender, "announced"))
        spec["mention"].append(m.sender.replaces.first)
    if c.renamed and m.date >= c.renamed[0] and not c.announced and not m.sender.enron:
        spec["notes"].append(f"Mention in passing that {c.name} is now {name}.")
        spec["told"].append((c, "announced"))
    if not m.facts:
        spec["numbers"] = allowed_numbers([d.desc(d.cur), d.cur.get("price_surface")], m.date)
        return spec
    if m.mode == "reply" and m.facts[0].stage in ("won", "lost") and len(m.facts) == 1 and rng.random() < S["subject_only_rate"]:
        spec["subject_only"] = True
        return spec
    role = "cp" if m.direction == "cp" else "enron"
    p = PROMISES.get((m.facts[0].stage, role))
    if p and rng.random() < S["commitment_rate"]:
        spec["promise"] = p
        spec["notes"].append(f"Also wrap one promise you make, to {p}, as ⟦C|...⟧.")
    sib = [o for o in others if o.introduced and o not in [f.deal for f in m.facts]]
    if sib and len(m.facts) == 1 and rng.random() < S["passing_mention_rate"]:
        o = rng.choice(sib)
        form = o.nickname or o.descriptor
        spec["distractor"] = (o, form[4:] if o.nickname else form)
        spec["numbers"] |= allowed_numbers([o.desc()], m.date)
        spec["notes"].append(f'Outside the marked sentence add a short aside such as "(not the {form})" or "separate from {form}".')
    return spec


def prompt_for(rng, S, m, spec, reason):
    persona = PERSONAS[m.sender.persona]
    head = [f"You are {m.sender.name}; sign as {m.sender.first} or not at all. Your writing style: {persona[0]}.",
            f"From: {fmt_people([m.sender])}", f"To: {fmt_people(m.to)}", f"Cc: {fmt_people(m.cc)}", f"Date: {m.date:%a %d %b %Y}"]
    if m.mode == "reply":
        head.append(f"It replies to this message (do not repeat it):\n<<<\n{m.prev.own.strip()[:700] or m.prev.subject}\n>>>")
    elif m.mode == "forward":
        head.append(f"It forwards this message with a short note on top; write only the note:\n<<<\n{m.prev.own.strip()[:700]}\n>>>")
    else:
        head.append("It starts a new thread.")
    name = m.cp.at(m.date)
    if m.kind == "chatter":
        topic = rng.choice([f"ask who covers {name[1]} while you are out next week", f"ask someone to set up a call on {name[1]} next week",
                            f"say you had lunch with the {name[1]} people and there is nothing new",
                            f"ask whether anyone has a current phone number for the {name[1]} contact"])
        body = [f"An internal note about {name[0]} ({name[1]}); background: {m.about.desc()}.",
                f"Its point: {topic}. Say nothing about where any deal stands. Use no ⟦ ⟧ markers."]
    elif m.kind == "nomination":
        month = (m.date.replace(day=1) + timedelta(days=32)).strftime("%B")
        body = [f"A routine monthly nomination under the existing deal ({m.about.desc()}): {m.about.cur['volume']} at "
                f"{m.about.cur['point']} for {month}.", "Say nothing about price or contract status. Use no ⟦ ⟧ markers."]
    elif spec["subject_only"]:
        body = [l.replace("ONE sentence", "an email subject line (under 9 words)") for b in spec["blocks"] for l in b]
        body.append("Write ONLY that subject line, nothing else.")
    else:
        body = [l for b in spec["blocks"] for l in b]
        if rng.random() < S["typo_rate"]:
            body.append("Include one small typo, never inside the exact words you were given.")
        if rng.random() < S["attachment_rate"]:
            body.append("Refer to an attachment.")
        habits = [h for h in HABITS if m.mode != "new" or "below" not in h]
        if rng.random() < 0.4:
            body.append(rng.choice(habits))
    if not spec["subject_only"]:
        marks = " and ".join(f"⟦{i}|...⟧" for i in range(1, len(m.facts) + 1))
        body = spec["notes"] + body + [f"Length: {persona[1]}."] + ([f"Before you finish: the sentence doing each deal's action is wrapped, {marks}."] if m.facts else [])
    if reason:
        body.append(f"Your last draft was rejected: {reason}. Fix that.")
    return "\n".join(head + [""] + body)


def check(text, m, spec):
    """(clean body, {label: quote}) or (None, reason)."""
    text = re.sub(r"\s*\|\s*⟧", "⟧", ascii_clean(text).strip())        # "⟦1|sentence|⟧" is a harmless echo of the label
    text = re.sub(r"(⟦[12C]\|[^⟦⟧|]*?)\|(?=\s*(?:\n|$))", r"\1⟧", text)   # and so is "⟦1|sentence|" at a line end
    text = re.sub(r"^(subject|from|to|cc|date)\s*:.*\n+", "", text, flags=re.I)
    found = MARK.findall(text)
    labels = [l for l, _ in found]
    want = [str(i) for i in range(1, len(m.facts) + 1)]
    if m.kind != "fact" and found:
        return None, "use no ⟦ ⟧ markers"
    for l in want:
        if labels.count(l) != 1:
            return None, f"wrap the fact sentence in ⟦{l}| ⟧ exactly once"
    if labels.count("C") > 1 or any(l not in want + ["C"] for l in labels):
        return None, "use only the markers asked for"
    if "⟦" in MARK.sub("", text) or "⟧" in MARK.sub("", text):
        return None, "a marker is unbalanced; write ⟦1|sentence⟧"
    quotes = {l: norm(q) for l, q in found}
    clean = MARK.sub(lambda x: x.group(2), text)
    for i, fs in enumerate(spec["facts"]):
        q = quotes[want[i]]
        if len(q) > 200:
            return None, "the marked sentence is over 200 characters"
        if len(words(q)) < 2:
            return None, "the marked sentence is too short to carry the fact"
        for form in fs["required"]:
            if form.lower() not in q.lower():
                return None, f'the marked sentence must contain "{form}"'
        for form in fs["forbidden"]:
            if has(q, form):
                return None, f'the marked sentence must not mention "{form}"'
    if "|" in clean:
        return None, "write ⟦1|sentence⟧ with one bar only, right after the label"
    for q in quotes.values():
        if norm(clean).lower().count(q.lower()) != 1:
            return None, "write the marked sentence once, not repeated"
    split = lambda t: [words(x) for x in re.split(r"[.?!\n]+", t or "") if len(words(x)) >= 5]
    rest, marked = split(MARK.sub("\n", text)), [words(q) for q in quotes.values() if len(words(q)) >= 5]
    before = [] if m.facts and m.facts[0].ev.get("fix") else split(m.prev.own if m.prev else "")
    over = lambda a, b: len(a & b) / min(len(a), len(b)) >= 0.6
    if any(over(a, b) for i, a in enumerate(rest) for b in rest[i + 1:] + marked + before):   # a quote may restate terms
        return None, "do not repeat a sentence, yours or the earlier message's"
    for n in NUM.findall(clean):
        if numkey(n) not in spec["numbers"]:
            return None, f'"{n}" is not a number you were given; use only the given numbers'
    lines = [l.strip(" -.,").lower() for l in clean.strip().split("\n") if l.strip()]
    others_first = {p.first.lower() for p in m.to + m.cc if p is not m.sender} - {m.sender.first.lower()}
    if lines and lines[-1] in others_first:
        return None, f"you are {m.sender.name}; sign as {m.sender.first} or not at all"
    if spec["subject_only"] and (len(lines) != 1 or len(clean.strip()) > 80):
        return None, "write only the subject line, one short line"
    for n in spec["foreign"]:
        if has(clean, n):
            return None, f'do not mention "{n}"'
    for n in spec["nick_in_body"]:
        if n.lower() not in clean.lower():
            return None, f'use the nickname "the {n}" somewhere'
    for n in ([] if spec["subject_only"] else spec["mention"]):
        if not has(clean, n):
            return None, f"say you took over from {n}"
    if "C" in quotes and len(words(quotes["C"])) < 2:
        quotes.pop("C")
    return clean, quotes


def mimo(budget, cfg, system, prompt, temperature):
    with budget.lock:
        if budget.calls >= budget.cap:
            return None
        budget.calls += 1
    body = {"model": MODEL, "temperature": temperature, "max_tokens": 700, "thinking": {"type": "disabled"},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}]}
    req = urllib.request.Request(f"{cfg['baseURL']}/chat/completions", json.dumps(body).encode(),
                                 {"Content-Type": "application/json", "Authorization": f"Bearer {cfg['apiKey']}"})
    for wait in (2, 5, 10, 20, 40, None):         # a rate limit or a server error is retried here, not charged as an attempt
        try:
            r = json.load(urllib.request.urlopen(req, timeout=120))
            break
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            print(f"  mimo error: {str(e)[:120]}", flush=True)
            if wait is None or getattr(e, "code", 429) not in (429, 500, 502, 503, 504):
                return None
            time.sleep(wait)
    u = r.get("usage") or {}
    with budget.lock:
        budget.prompt += u.get("prompt_tokens", 0)
        budget.completion += u.get("completion_tokens", 0)
    return r["choices"][0]["message"].get("content") or ""


def fake_llm(m, spec):
    """--dry-run: a body that passes the check, for testing the plumbing without MiMo."""
    if m.kind != "fact":
        return "nothing new here, talk monday"
    out = [f"⟦{i}|{' '.join(fs['required']) or 'that one'} {fs['fact'].stage}⟧" for i, fs in enumerate(spec["facts"], 1)]
    return "\n".join(out[:1] if spec["subject_only"] else out + [f"the {n} it is" for n in spec["nick_in_body"]] + ["thanks"])


# ---------------------------------------------------------------- rendering

def wrap(s):
    out = []
    for line in s.split("\n"):
        out += textwrap.wrap(line, 76, break_long_words=False, break_on_hyphens=False) if len(line) > 78 else [line]
    return "\n".join(out)


def phone(person):
    r = random.Random(person.email)       # one number per person, every message
    return f"({r.choice(['520', '541', '702', '505', '208', '775'])}) {r.randint(200, 899)}-{r.randint(1000, 9999)}"


def signature(rng, S, m, cp_ref):
    s, when = m.sender, m.date
    if s.enron:
        if s.role == "senior counsel" and rng.random() < S["signature_rate"]:
            return (f"\n\n{s.name}\nSenior Counsel\nEnron North America Corp.\n1400 Smith, EB38{rng.randint(10, 99)}\n"
                    f"Houston, TX  77002\nPhone:  (713) 853-{rng.randint(1000, 9999)}\nemail:  {s.email}")
        return f"\nx3-{rng.randint(1000, 9999)}" if rng.random() < S["signature_rate"] / 3 else ""
    name = m.cp.at(when)[0]
    formerly = f"\n(formerly {m.cp.name})" if m.cp.renamed and when >= m.cp.renamed[0] and not m.cp.announced else ""
    if cp_ref == "domain_only":
        return f"\n\n{s.name}\n{phone(s)}\n{s.addr(when)}"
    if (cp_ref in ("canonical", "alias", "misspelt") and rng.random() < S["signature_rate"]) or formerly:
        return f"\n\n{s.name}\n{s.role.title()}\n{name}{formerly}\n{phone(s)}\n{s.addr(when)}"
    return ""


def sent_outlook(d):
    return d.strftime("%A, %B %d, %Y ") + d.strftime("%I:%M %p").lstrip("0")


def notes_stamp(d, secs=False):
    return d.strftime("%m/%d/%Y %I:%M:%S %p" if secs else "%m/%d/%Y %I:%M %p")


def outlook_block(p, gt):
    a = lambda ps: ", ".join(x.addr(p.date) for x in ps)
    cc = f"Cc: {a(p.cc)}\n" if p.cc else ""
    block = (f"-----Original Message-----\nFrom: {p.sender.addr(p.date)} [mailto:{p.sender.addr(p.date)}]\n"
             f"Sent: {sent_outlook(p.date)}\nTo: {a(p.to)}\n{cc}Subject: {p.subject}\n\n\n{p.full}")
    if gt:
        block = "\n".join("> " + l if l else ">" for l in block.split("\n"))
    return "\n\n\n" + block


def notes_header(p):
    if p.sender.enron:
        return (f"\t{p.sender.name}\n\t{notes_stamp(p.date)}\n\t\t \n\t\t To: {', '.join(x.name for x in p.to)}\n"
                f"\t\t cc: {', '.join(x.name for x in p.cc)}\n\t\t Subject: {p.subject}\n\n")
    return (f"{p.sender.xfrom(p.date)} on {notes_stamp(p.date, True)}\nTo: {', '.join(x.addr(p.date) for x in p.to)}\n"
            f"cc:  {', '.join(x.addr(p.date) for x in p.cc)}\nSubject: {p.subject}\n\n\n")


def render_body(rng, S, m, clean, cp_ref, subject_only):
    m.clean = clean.strip()
    if subject_only:
        m.subject, own = clean.strip().split("\n")[0], ""
    else:
        own = wrap(clean.strip()) + signature(rng, S, m, cp_ref)
        if m.facts and m.facts[0].stage in ("proposal", "negotiating") and rng.random() < S["attachment_rate"]:
            name = f"{m.cp.at(m.date)[1].replace(' ', '_')}_{rng.choice(['draft', 'termsheet', 'indication', 'redline'])}{rng.randint(1, 4)}.doc"
            own += f"\n\n(See attached file: {name})" if m.sender.enron else f"\n\n - {name}"
        if not m.sender.enron and m.cp.disclaimer:
            own += "\n\n\n" + wrap("This e-mail and any attachments are confidential and intended solely for the "
                                    "addressee. If you have received it in error please notify the sender and delete it.")
    m.own = own
    if m.mode == "reply":
        m.full = own + ("\n\n\n\n" + notes_header(m.prev) + m.prev.full if m.sender.enron else
                        outlook_block(m.prev, m.cp.quote_gt))
    elif m.mode == "forward":
        line = f"---------------------- Forwarded by {m.sender.name}/HOU/ECT on {notes_stamp(m.date)} "
        m.full = own + f"\n\n{line}\n---------------------------\n\n\n{notes_header(m.prev)}{m.prev.full}"
    else:
        m.full = own


def headers(m):
    tz = m.date.strftime("%Z")
    date = m.date.strftime("%a, ") + str(m.date.day) + m.date.strftime(f" %b %Y %H:%M:00 %z ({tz})")
    a = lambda ps: ", ".join(p.addr(m.date) for p in ps)
    xto = lambda ps: ", ".join(p.name if p.enron else p.addr(m.date) for p in ps)
    h = [f"Message-ID: <{m.msgid}>", f"Date: {date}", f"From: {m.sender.addr(m.date)}", f"To: {a(m.to)}", f"Subject: {m.subject}"]
    if m.cc:
        h.append(f"Cc: {a(m.cc)}")
    h += ["Mime-Version: 1.0", "Content-Type: text/plain; charset=us-ascii", "Content-Transfer-Encoding: 7bit"]
    if m.cc:
        h.append(f"Bcc: {a(m.cc)}")
    folder = m.folder.replace("_", " ").capitalize()
    h += [f"X-From: {m.sender.xfrom(m.date)}", f"X-To: {xto(m.to)}", f"X-cc: {xto(m.cc)}", "X-bcc: ",
          f"X-Folder: \\Kim_Ward_Nov2001\\Notes Folders\\{folder}", "X-Origin: WARD-K", "X-FileName: kward.nsf"]
    return "\n".join(h), date


# ---------------------------------------------------------------- one counterparty, start to finish

def run_counterparty(seed, S, c, deals, staff, cps, budget, cfg, dry):
    rng = random.Random(f"{seed}-{c.id}")
    msgs = plan_messages(rng, S, c, deals)
    done, log = [], []
    for m in msgs:
        fix = m.facts[0].ev.get("fix") if m.facts else None
        if fix:                                       # a correction answers its own slip
            orig = fix.get("msg")
            if orig is None:
                log.append(f"DROP {m.date:%Y-%m-%d} correction of a message that was not written")
                continue
            m.direction, m.sender, m.to, m.cc, m.prev, m.mode = orig.direction, orig.sender, orig.to, orig.cc, orig, "reply"
            m.subject = ("Re: " if m.sender.enron else "RE: ") + base_subject(orig.subject)
        elif m.kind == "fact":
            th = m.thread
            if len(m.facts) == 1:
                m.direction = choose_direction(rng, S, m.facts[0].stage)
            else:                                     # two deals in one message: to or from the customer; only Enron offers
                m.direction = "out" if any(f.stage == "proposal" for f in m.facts) else rng.choice(["out", "cp"])
            ext = [x for x in (th["msgs"] if th else []) if x.direction != "internal"]
            if ext and m.direction == ext[-1].direction and m.facts[0].stage != "proposal" and rng.random() < 0.7:
                m.direction = "cp" if m.direction == "out" else "out"   # the other side answers, mostly
            if th and th["msgs"] and len(m.facts) == 1 and m.facts[0].stage != "proposal" and rng.random() < S["forward_rate"]:
                m.direction = "internal"                                  # passed on to colleagues as a forward
            if m.direction == "internal":
                m.prev = th["msgs"][-1] if th and th["msgs"] else None
                m.mode = "forward" if m.prev else "new"
            else:
                m.prev = ext[-1] if ext else None
                m.mode = "reply" if m.prev else "new"
            fill_roles(rng, m, staff)
            if m.mode == "new":
                if th is not None:
                    th["subject"] = th["subject"] if th["subject"] is not None else subject_for(rng, th["deal"], m.date)
                    m.subject = th["subject"]
                else:
                    m.subject = rng.choice(["couple of things", c.at(m.date)[1], "", "positions"])
            elif m.mode == "reply":
                m.subject = ("Re: " if m.sender.enron else "RE: ") + base_subject(m.prev.subject)
            else:
                m.subject = "FW: " + base_subject(m.prev.subject) if rng.random() < 0.5 else m.prev.subject
        else:
            m.direction = "internal" if m.kind == "chatter" else "cp"
            fill_roles(rng, m, staff)
            m.subject = (f"{m.date.replace(day=1) + timedelta(days=32):%B} Nominations" if m.kind == "nomination"
                         else rng.choice([c.at(m.date)[1], "", f"{c.at(m.date)[1]} coverage", "fyi"]))
        spec = build_spec(rng, S, m, deals, cps)
        reason, result = None, None
        for attempt in range(4):
            prompt = prompt_for(rng, S, m, spec, reason)
            text = fake_llm(m, spec) if dry else mimo(budget, cfg, rng.choice(CARDS) + RULES, prompt, round(rng.uniform(*S["temperature"]), 2))
            if DEBUG and text is not None:                # WORLD_DEBUG=<file>: every attempt, its prompt and verdict
                with budget.lock, open(DEBUG, "a") as fh:
                    fh.write(json.dumps({"cp": c.id, "date": str(m.date), "prompt": prompt, "text": text, "verdict": check(text, m, spec)[1]
                                         if check(text, m, spec)[0] is None else "ok"}) + "\n")
            if text is None:
                reason = "no answer"
                if budget.calls >= budget.cap:
                    break
                continue
            clean, quotes = check(text, m, spec)
            if clean is None:
                reason = quotes
                continue
            result = (clean, quotes)
            break
        stages = ",".join(f"{f.deal.id}:{f.stage}" for f in m.facts) or m.kind
        if result is None:
            log.append(f"DROP {m.date:%Y-%m-%d} {stages} ({reason})")
            continue
        clean, quotes = result
        render_body(rng, S, m, clean, spec["facts"][0]["fact"].cp_ref if spec["facts"] else "implicit", spec["subject_only"])
        for obj, attr in ([] if spec["subject_only"] else spec["told"]):
            setattr(obj, attr, True)
        if m.thread is not None:
            m.thread["msgs"].append(m)
        m.deals |= {f.deal for f in m.facts} | ({m.thread["deal"]} if m.thread else set()) | ({m.about} if m.about else set())
        m.deals |= m.prev.deals if m.prev else set()
        if spec["distractor"] and spec["distractor"][1].lower() in clean.lower():
            m.deals.add(spec["distractor"][0])
            m.distractor = spec["distractor"][0]
        for i, f in enumerate(m.facts, 1):
            d, st, last = f.deal, f.stage, f.deal.last_stage
            since = d.n_msgs - d.nick_at if d.nick_at is not None else None
            if d.nickname and d.nickname[4:] in spec["nick_in_body"] and d.nick_at is None:
                d.nick_at, since = d.n_msgs, 0
            flags = {k: k in f.ev["dyn"] for k in FLAGS}
            flags |= {"revive": last == "lost" and st != "lost", "cancel": last == "won" and st == "lost" and "superseded" not in f.ev["dyn"],
                      "regress": bool(last) and "lost" not in (st, last) and ORDER[st] < ORDER[last],
                      "rename": bool(c.renamed) and m.date >= c.renamed[0],
                      "turnover": any(p.replaces for p in [m.sender] + m.to + m.cc),
                      "handoff": bool(d.handoff) and m.date >= d.handoff[0],
                      "correction": "correction" in f.ev["dyn"] or bool(f.ev["over"])}
            f.quote = quotes[str(i)]
            f.tags = {"kind": "stage_update", "deal": d.id, "stage": st,
                      "reference_form": "canonical" if f.deal_ref == "intro" else f.deal_ref, "counterparty_form": f.cp_ref,
                      "internal": m.direction == "internal", "forwarded": m.mode == "forward", "quoted_reply": m.mode == "reply",
                      "several_deals_in_message": len(m.facts) > 1, "sibling_deals": sum(x.cp is c for x in deals) - 1,
                      "messages_since_nickname_introduced": since, **flags}
            d.introduced, d.last_stage = True, st
            d.cur.update(f.ev["set"])
            f.ev["msg"] = m
        for d in m.deals:
            d.n_msgs += 1
        if spec["promise"] and "C" in quotes:
            stated = DUE.search(quotes["C"])
            m.commitment = {"what": spec["promise"], "due": stated.group(0) if stated else None, "quote": quotes["C"]}
        m.msgid = f"{rng.randint(1000000, 29999999)}.{1075850000000 + rng.randint(0, 9999999)}.JavaMail.evans@thyme"
        done.append(m)
        log.append(f"ok   {m.date:%Y-%m-%d} {stages} [{m.mode}{' tries=' + str(attempt + 1) if attempt else ''}]"
                   + (f" last rejection: {reason}" if attempt else ""))
    print(f"{c.id}: {len(done)} messages\n  " + "\n  ".join(log), flush=True)
    return done


# ---------------------------------------------------------------- output

def write_out(out, seed, S, hay, cps, deals, msgs, budget, planned, dyn):
    rng = random.Random(f"{seed}-place")
    story = out / "story"
    if out.exists():
        shutil.rmtree(out)
    story.mkdir(parents=True)
    msgids, hay_ids = {}, set()
    for folder, name, text in hay:
        (story / folder).mkdir(exist_ok=True)
        shutil.copyfile(WARD / "all" / folder / name, story / folder / name)
        mid = re.search(r"^Message-ID:\s*<([^>]+)>", text, re.M)
        msgids[f"{folder}/{name}"] = mid.group(1) if mid else None
        hay_ids.add(mid.group(1) if mid else None)
    for m in msgs:
        m.folder = m.cp.id
    facts_msgs = [m for m in msgs if m.facts]
    for i, m in enumerate(rng.sample(facts_msgs, min(S["in_real_folders"], len(facts_msgs)))):
        m.folder = S["hay_folders"][i % len(S["hay_folders"])]
    by_folder = {}
    for m in msgs:
        by_folder.setdefault(m.folder, []).append(m)
    for folder, ms in by_folder.items():
        start = 1 + max((int(n.rstrip(".")) for f, n, _ in hay if f == folder), default=0)
        for k, m in enumerate(sorted(ms, key=lambda x: x.date, reverse=True)):
            m.file = f"{start + k}."
        (story / folder).mkdir(exist_ok=True)
    for m in msgs:
        while m.msgid in hay_ids:
            m.msgid = "1" + m.msgid
        h, m.date_header = headers(m)
        (story / m.folder / m.file).write_text(h + "\n\n" + m.full.rstrip() + "\n")
        msgids[f"{m.folder}/{m.file}"] = m.msgid
        for d in m.deals:
            d.files.append(m.file if m.folder == d.cp.id else f"{m.folder}/{m.file}")
    gold, tags = out / "gold", []
    gold.mkdir()
    spec = f"GENERATED v2 seed {seed}"
    for folder, ms in sorted(by_folder.items()):
        fictional = folder in {c.id for c in cps}
        people, pid = [], {}
        for m in sorted(ms, key=lambda x: x.date):
            for p in [m.sender] + m.to + m.cc:
                if id(p) not in pid:
                    pid[id(p)] = f"p{len(pid) + 1}"
                    people.append({"id": pid[id(p)], "name": p.name, "emails": [], "company": p.company, "internal": p.enron})
                rec = people[int(pid[id(p)][1:]) - 1]
                rec["emails"] += [p.addr(m.date)] if p.addr(m.date) not in rec["emails"] else []
        comps = [{"id": "enron", "name": "Enron", "domains": ["enron.com"]}]
        for c in cps:
            if c.id in {m.cp.id for m in ms}:
                comps.append({"id": c.id, "name": c.renamed[1] if c.renamed else c.name,
                              "domains": [c.domain] + ([c.renamed[4]] if c.renamed else [])}
                             | ({"aka": [c.name, c.alias]} if c.renamed else {}))
        gdeals = [] if not fictional else [
            {"id": d.id, "counterparty": d.cp.id, "description": d.desc(), "amount_usd": d.amount,
             "price": d.cur.get("price"), "kind": d.kind, "messages": d.files} for d in deals if d.cp.id == folder]
        ups, coms = [], []
        for m in sorted(ms, key=lambda x: x.date):
            for f in m.facts:
                ups.append({"deal": f.deal.id, "stage": f.stage, "quote": f.quote, "file": m.file, "date": m.date_header})
                tags.append({"folder": folder, "file": m.file, **f.tags})
            if m.commitment:
                coms.append({"person": pid[id(m.sender)], **m.commitment, "file": m.file, "date": m.date_header})
                tags.append({"folder": folder, "file": m.file, **m.facts[0].tags, "kind": "commitment", "stage": None,
                             "person": pid[id(m.sender)]})
        files = sorted((m.file for m in ms), key=lambda n: int(n.rstrip(".")))
        busy = {u["file"] for u in ups} | {c_["file"] for c_ in coms}
        g = {"folder": folder, "spec": spec if fictional else spec + "; only the generated files are labelled, the real mail here is not",
             "files_read": files, "files_with_nothing": [f for f in files if f not in busy], "people": people,
             "companies": comps, "deals": gdeals, "stage_updates": ups, "commitments": coms}
        (gold / f"{folder}.json").write_text(json.dumps(g, indent=1) + "\n")
    (out / "tags.jsonl").write_text("".join(json.dumps(t) + "\n" for t in tags))
    (out / "msgids.json").write_text(json.dumps(msgids, indent=1, sort_keys=True) + "\n")
    (out / "bodies.json").write_text(json.dumps([m.clean for m in msgs], indent=0) + "\n")
    corpus_id, home = out.name, str(Path.home())
    path = str(out / "story")
    path = "~" + path[len(home):] if path.startswith(home) else path
    recipe = RECIPE.read_text()
    recipe = re.sub(r'(?m)^id = "ward-story"$', f'id = "{corpus_id}"', recipe, count=1)
    recipe = re.sub(r'(?m)^name = "CRM from a mailbox[^"\n]*"$',
                    f'name = "CRM from a mailbox: a generated deal world (seed {seed}) buried in Kim Ward\'s mail"', recipe, count=1)
    recipe = re.sub(r'(?m)^path = "~/.svrnmesh/bench-corpora/enron-ward/story"$', f'path = "{path}"', recipe, count=1)
    (out / "recipe.toml").write_text(recipe)
    stage_tags = [t for t in tags if t["kind"] == "stage_update"]
    manifest = {
        "seed": seed, "spec": spec, "model": MODEL, "settings": S,
        "counts": {"companies": len(cps), "kinds": dict(Counter(c.kind for c in cps)), "deals": len(deals),
                   "deal_types": dict(Counter(d.type for d in deals)), "stage_updates_planned": planned,
                   "stage_updates": len(stage_tags), "dropped": planned - len(stage_tags),
                   "commitments": sum(1 for m in msgs if m.commitment), "messages_generated": len(msgs),
                   "messages_multi_deal": sum(len(m.facts) > 1 for m in msgs),
                   "messages_forward": sum(m.mode == "forward" for m in msgs), "messages_reply": sum(m.mode == "reply" for m in msgs),
                   "messages_passing_mention": sum(1 for m in msgs if getattr(m, "distractor", None)),
                   "messages_chatter": sum(m.kind == "chatter" for m in msgs),
                   "messages_nomination": sum(m.kind == "nomination" for m in msgs),
                   "messages_in_real_folders": sum(m.folder != m.cp.id for m in msgs), "hay_messages": len(hay),
                   "updates_by_flag": {k: sum(t[k] for t in stage_tags) for k in FLAGS},
                   "updates_by_reference_form": dict(Counter(t["reference_form"] for t in stage_tags)),
                   "updates_by_counterparty_form": dict(Counter(t["counterparty_form"] for t in stage_tags)),
                   "mimo_calls": budget.calls, "tokens": {"prompt": budget.prompt, "completion": budget.completion},
                   "mail_tokens_est": {"generated": sum((story / m.folder / m.file).stat().st_size for m in msgs) // 4,
                                       "hay": sum(len(t) for _, _, t in hay) // 4}},
        "dynamics": [{k: (v.id if isinstance(v, Deal) else v.strftime("%Y-%m-%d %H:%M") if isinstance(v, datetime) else v) for k, v in e.items()} for e in dyn],
        "score_folders": sorted(by_folder),
        "score": "research/ontology-apps/loop/loop.sh <arm> <corpus id>",
        "commitments_note": "planned promises only; a promise the writer added unasked is not labelled",
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=1, default=str) + "\n")
    return manifest


def validate(out):
    """Every gold file exists, every quote is in its file (whitespace-normalized), every file has a Message-ID."""
    out = Path(out).expanduser()
    msgids = json.load(open(out / "msgids.json"))
    n_q = n_ok = n_files = 0
    bad = []
    deals = {d["id"] for g in (out / "gold").glob("*.json") for d in json.load(open(g))["deals"]}
    for g in sorted((out / "gold").glob("*.json")):
        gold = json.load(open(g))
        folder = gold["folder"]
        for f in gold["files_read"]:
            n_files += 1
            bad += [f"missing {folder}/{f}"] if not (out / "story" / folder / f).is_file() else []
        for kind in ("stage_updates", "commitments"):
            for u in gold[kind]:
                n_q += 1
                p = out / "story" / folder / u["file"]
                if p.is_file() and norm(u["quote"]) in norm(p.read_text()):
                    n_ok += 1
                else:
                    bad.append(f"quote not in {folder}/{u['file']}: {u['quote'][:60]}")
                if kind == "stage_updates" and u["deal"] not in deals:
                    bad.append(f"unknown deal {u['deal']}")
    story = [p for p in (out / "story").rglob("*.") if p.is_file()]
    bad += [f"no msgid {p.parent.name}/{p.name}" for p in story if not msgids.get(f"{p.parent.name}/{p.name}")]
    n_tags = sum(1 for _ in open(out / "tags.jsonl"))
    bad += [f"tags.jsonl has {n_tags} lines for {n_q} labels"] if n_tags != n_q else []
    return {"world": out.name, "gold_files": len(list((out / "gold").glob("*.json"))), "files_checked": n_files,
            "quotes": n_q, "quotes_verbatim": n_ok, "story_files": len(story), "msgids": len(msgids), "tags": n_tags, "problems": bad}


def heterogeneity(root):
    """How much the worlds' written bodies share, and what MiMo repeats: a templating generator shows up here."""
    worlds = {p.parent.name: (json.load(open(p)), json.load(open(p.parent / "manifest.json")))
              for p in sorted(root.glob("world-s*/bodies.json"))}

    def grams(texts, n):
        out = Counter()
        for t in texts:
            w = re.findall(r"[a-z0-9']+", t.lower())
            out.update(tuple(w[i:i + n]) for i in range(len(w) - n + 1))
        return out
    g5 = {k: set(grams(b, 5)) for k, (b, _) in worlds.items()}
    shared = {k: round(len(g5[k] & set().union(*(g5[o] for o in g5 if o != k))) / max(1, len(g5[k])), 3) for k in g5}
    g4 = grams([t for b, _ in worlds.values() for t in b], 4)
    per = {k: set(grams(b, 4)) for k, (b, _) in worlds.items()}
    report = {"worlds": {k: {"settings": man["settings"], "counts": man["counts"], "fivegram_share_with_other_worlds": shared[k]}
                         for k, (_, man) in worlds.items()},
              "fivegram_share_with_other_worlds": shared,
              "top_fourgrams": [{"gram": " ".join(g), "count": n, "worlds": sum(g in s for s in per.values())}
                                for g, n in g4.most_common(20)]}
    (root / "heterogeneity.json").write_text(json.dumps(report, indent=1, default=str) + "\n")
    print("5-gram share with other worlds:", json.dumps(shared))
    print("top 4-grams:", "; ".join(f"{t['gram']} ({t['count']}, {t['worlds']}w)" for t in report["top_fourgrams"]))


def taken_names(root, skip):
    """Names the other worlds already use, so no two worlds share a company or a person."""
    used = set()
    for p in root.glob("world-s*/gold/*.json"):
        if p.parent.parent.name in skip:
            continue
        g = json.load(open(p))
        for c in g["companies"]:
            used |= {x.lower() for x in [c["name"], *c.get("aka", []), *c["domains"]] + [dbase(x) for x in c["domains"]] if c["id"] != "enron"}
        for q in g["people"]:
            used |= {q["name"].lower(), q["name"].split()[-1].lower(), *[e.lower() for e in q["emails"]]} if q["name"] != "Kim Ward" else set()
    return used


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seeds", default="1")
    ap.add_argument("--out", help="with one seed: write the world here instead of <root>/world-s<seed>")
    ap.add_argument("--root", default=str(ROOT))
    ap.add_argument("--max-calls", type=int, default=400, help="MiMo calls per world")
    ap.add_argument("--dry-run", action="store_true", help="template bodies instead of MiMo")
    ap.add_argument("--check", action="store_true", help="only validate written worlds and redo heterogeneity.json")
    a = ap.parse_args()
    root, seeds = Path(a.root).expanduser(), [int(s) for s in a.seeds.split(",")]
    outs = {s: Path(a.out).expanduser() if a.out and len(seeds) == 1 else root / f"world-s{s}" for s in seeds}
    cfg = None if a.dry_run or a.check else json.load(open(OPENCODE))["provider"]["mimo"]["options"]
    used, ok = taken_names(root, {o.name for o in outs.values()}), True
    for seed, out in outs.items():
        if not a.check:
            rng = random.Random(seed)
            S = draw_settings(rng)
            hay = load_hay(S["hay_folders"], S["hay_per_folder"])
            cps, staff, deals, dyn = make_world(rng, S, "\n".join(t for _, _, t in hay).lower(), used)
            planned = sum(1 for d in deals for _ in d.events)
            print(f"world-s{seed}: {len(cps)} companies, {len(deals)} deals, {planned} stage moves, {len(dyn)} changes; "
                  f"hay {len(hay)} from {S['hay_folders']}", flush=True)
            budget = Budget(a.max_calls)
            with ThreadPoolExecutor(max_workers=min(6, len(cps))) as ex:   # more draws 429s from the API
                parts = list(ex.map(lambda c: run_counterparty(seed, S, c, [d for d in deals if d.cp is c], staff, cps,
                                                               budget, cfg, a.dry_run), cps))
            manifest = write_out(out, seed, S, hay, cps, deals, [m for p in parts for m in p], budget, planned, dyn)
            print(json.dumps(manifest["counts"]))
        v = validate(out)
        ok &= not v["problems"]
        print(json.dumps(v))
    if not a.out:
        heterogeneity(root)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
