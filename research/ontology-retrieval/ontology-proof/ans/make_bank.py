#!/usr/bin/env python3
"""The ANS bank. THE RULE, fixed before any yield was counted and before any arm ran:

* K1: one question per (hoard, list type) over truth/hoards.json (CoinHoards IGCH; never the
  monographs, never anyone's memory), where
    Q1  the list has 4..40 members (the pseudo-member nomisma `uncertain_value` removed),
    Q2  some paragraph of the stripped text ANCHORS the hoard: it prints "IGCH <n>", or the
        findspot NAME within 30 characters of the word hoard/find/deposit or of the hoard's
        discovery year (the books write "Asia Minor 1964", "the Demanhur hoard"), and
    Q3  >= 4 members are LOCAL: they occur in a hoard paragraph, i.e. an anchoring paragraph
        or one inside a section whose heading anchors it.
  AMENDED ONCE, after the first count (39 rows) and before any system output existed: Q2/Q3
  first took any paragraph NAMING the findspot, which made all 680 paragraphs that say
  "Asia Minor" (or Egypt, Greece, Corinth) hoard paragraphs and `local` meaningless there.
  The text is read only to attest (as attest.py does), never to decide a hoard's contents.
* One template per list type (TEMPLATES). No IGCH number in the question: 22 of 25 works
  predate IGCH (1973) and name hoards by findspot and year, so the question does too; the
  three later works print "IGCH n" beside the name. The number is in `notes` and gold/.
* NAME = the IGCH findspot description up to its first "," or "(", quotes dropped.
  `expected_facts` = every member's nomisma label cut at its first "," / " of " / " in "
  ("Alexander III of Macedon" -> "Alexander III", "Antioch, Syria" -> "Antioch"): the Rust
  scorer needs EVERY >=3-char token of a fact, so an OR-group would read as an AND.
* Two hoards that produce the same question text are both dropped, recorded in gold/.
* SCATTER, measured per row and never used to select: over the hoard paragraphs, how many
  hold >= 1 local member, and the greedy minimum number of paragraphs covering all local
  members. cover == 1 is labelled `single_passage` so it can be reported apart.
* K0: 15 hand-written lookups in k0.toml, fact copied from one line; this script REFUSES a
  row whose fact is not on the line its `notes` cites.

    python3 make_bank.py       # writes bank.src.toml, gold/*.json, gold/_summary.json
"""
import collections, json, pathlib, re, sys, tomllib, unicodedata

HERE = pathlib.Path(__file__).resolve().parent
LO, HI, MIN_LOCAL = 4, 40, 4
TEMPLATES = {
    "mints": "Which mints are represented among the coins of the {name} hoard{when}?",
    "rulers": "Which rulers' coinages are represented in the {name} hoard{when}?",
    "denominations": "Which coin denominations are represented in the {name} hoard{when}?",
}
TYPE = {"mints": "mint", "rulers": "ruler", "denominations": "coin.denomination"}


def fold(s):
    return "".join(c for c in unicodedata.normalize("NFKD", s) if not unicodedata.combining(c)).casefold()


def rx(s):
    return re.compile(r"(?<!\w)" + re.escape(fold(s)) + r"(?!\w)")


def short(label):
    return re.split(r",| of | in ", label)[0].strip()


def paragraphs():
    """[(work, line, folded text, heading level or 0)] for every block of every work."""
    out = []
    for f in sorted((HERE / "text").glob("nnan*.md")):
        line = 1
        for block in f.read_text(encoding="utf8").split("\n\n"):
            m = re.match(r"(#+) ", block)
            out.append((f.stem, line, fold(block), len(m.group(1)) if m else 0))
            line += block.count("\n") + 2
    return out


def hoard_paragraphs(h, name, paras):
    year = re.search(r"\d{4}", h["discovery"] or "")
    near = "hoards?|find|deposit" + (f"|{year.group(0)}" if year else "")
    n = re.escape(fold(name))
    pat = re.compile(rf"(?<!\w)igch {h['igch']}(?!\d)" + (rf"|(?<!\w){n}(?!\w).{{0,30}}?(?<!\w)({near})(?!\w)"
                     rf"|(?<!\w)(hoards?|find|deposit)(?!\w).{{0,30}}?(?<!\w){n}(?!\w)" if name else ""))
    keep, until = [], None                    # `until`: heading level that closes the open section
    for i, (work, _, text, level) in enumerate(paras):
        if level and until and level <= until or (until and work != paras[i - 1][0]):
            until = None
        if pat.search(text):
            keep.append(i)
            until = level if level and not until else until
        elif until:
            keep.append(i)
    return keep


def cover(sets, universe):
    left, n = set(universe), 0
    while left and (best := max(sets, key=lambda s: len(s & left), default=set())) & left:
        left -= best; n += 1
    return n


def main():
    hoards = json.loads((HERE / "truth/hoards.json").read_text())["hoards"]
    paras = paragraphs()
    (HERE / "gold").mkdir(exist_ok=True)
    for stale in (HERE / "gold").glob("list-*.json"):
        stale.unlink()
    rows, absent = [], []
    for h in hoards:
        name = re.split(r"[,(]", h["findspot"] or "")[0].strip().strip('"“”')
        when = "".join(f"{sep}{h[k]}" for sep, k in ((", found ", "discovery"), (", buried about ", "deposit")) if h[k])
        hp = hoard_paragraphs(h, name, paras)
        for kind, template in TEMPLATES.items():
            members = [m for m in h[kind] if m != "Uncertain value"]
            rec = {"id": f"list-igch{h['igch']:04d}-{kind}", "igch": h["igch"], "uri": h["uri"], "findspot": h["findspot"],
                   "name": name, "kind": kind, "works": h["works"], "members": members, "facts": [short(m) for m in members]}
            if not LO <= len(members) <= HI:
                absent.append({**rec, "reason": f"Q1: {len(members)} members"}); continue
            if not hp:
                absent.append({**rec, "reason": f"Q2: no paragraph anchors {name!r}"}); continue
            pats = {m: rx(short(m)) for m in members}
            where = {i: {m for m, p in pats.items() if p.search(paras[i][2])} for i in hp}
            local = sorted(set().union(*where.values()))
            rec.update(members_in_corpus=sorted(m for m, p in pats.items() if any(p.search(t[2]) for t in paras)),
                       members_local=local, hoard_paragraphs=len(hp),
                       paragraphs_with_a_member=sum(bool(v) for v in where.values()),
                       cover=cover(list(where.values()), local),
                       works_naming_hoard=sorted({paras[i][0] for i in hp}))
            rec["single_passage"] = rec["cover"] == 1
            if len(local) < MIN_LOCAL:
                absent.append({**rec, "reason": f"Q3: {len(local)} local members"}); continue
            rec["question"] = template.format(name=name, when=when)
            rows.append(rec)
    dupes = {q for q, n in collections.Counter(r["question"] for r in rows).items() if n > 1}
    absent += [{**r, "reason": "same question text as another hoard"} for r in rows if r["question"] in dupes]
    rows = [r for r in rows if r["question"] not in dupes]

    k0 = tomllib.loads((HERE / "k0.toml").read_text())["questions"]
    for q in k0:
        work, line = re.match(r"(\S+\.md):(\d+)", q["notes"]).groups()
        text = (HERE / "text" / work).read_text(encoding="utf8").split("\n")[int(line) - 1]
        if not all(f.casefold() in text.casefold() for f in q["expected_facts"]):
            sys.exit(f"REFUSED: {q['id']}: fact not on {work}:{line}")

    esc = lambda s: json.dumps(s, ensure_ascii=False)
    out = ['[bank]', 'name = "ontology-proof-ans-v1"', 'corpus = "ei7-ans"',
           f'description = "K1 hoard-contents lists (truth: CoinHoards IGCH, held out) and {len(k0)} K0 lookups over ANS monographs (make_bank.py)."', ""]
    for r in rows:
        out += ["[[questions]]", f'id = "{r["id"]}"', 'category = "unattested"', f"question = {esc(r['question'])}",
                f"expected_facts = {esc(r['facts'])}",
                f'notes = "{TYPE[r["kind"]]}; IGCH {r["igch"]}; local {len(r["members_local"])}/{len(r["members"])}; '
                f'cover {r["cover"]}{"; single_passage" if r["single_passage"] else ""}; gold/{r["id"]}.json"', ""]
        (HERE / "gold" / f"{r['id']}.json").write_text(json.dumps(r, indent=1, ensure_ascii=False) + "\n")
    for q in k0:
        out += ["[[questions]]", f'id = "{q["id"]}"', 'category = "unattested"', f"question = {esc(q['question'])}",
                f"expected_facts = {esc(q['expected_facts'])}", f"notes = {esc(q['notes'])}", ""]
    (HERE / "bank.src.toml").write_text("\n".join(out), encoding="utf8")
    by = lambda rs, k: dict(sorted(collections.Counter(r[k] for r in rs).items()))
    summary = {"k1_rows": len(rows), "k1_by_kind": by(rows, "kind"), "k0_rows": len(k0),
               "single_passage_rows": sum(r["single_passage"] for r in rows), "cover_distribution": by(rows, "cover"),
               "paragraphs_with_a_member_distribution": by(rows, "paragraphs_with_a_member"),
               "local_over_members": [f"{len(r['members_local'])}/{len(r['members'])}" for r in rows],
               "hoards_with_a_row": len({r["igch"] for r in rows}),
               "absent_by_reason": dict(collections.Counter(a["reason"].split(":")[0] for a in absent)), "absent": absent}
    (HERE / "gold" / "_summary.json").write_text(json.dumps(summary, indent=1, ensure_ascii=False) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k not in ("absent", "local_over_members")}, indent=1))


if __name__ == "__main__":
    main()
