#!/usr/bin/env python3
"""The frozen bank for order ei7-prove-ontology. THE RULE, fixed before any arm ran:

* K1: one question per (service, list type) where the service's OWN policy text
  publishes that list with >= 5 items. One template per type (TYPES), applied
  mechanically. Gold never comes from the atlas, atoms, or anyone's memory.
* A list is found by `recensus/gold_lists.py`'s rule, imported not copied: a
  qualifying heading's span; items are list items or bolded lead-ins, normalised
  by `decisions.norm`, prose over `decisions.MAX_TRUTH_WORDS` dropped and
  recorded. Only the document and heading pattern change per type. Reaches
  beyond it, all type-blind, each for a shape a service actually publishes in: a
  TABLE's first column when its label names the type (Spotify); an ITALIC
  lead-in, in a list item or a cell of a header-less layout table (Reddit); a
  SUB-HEADING inside the span (YouTube, Facebook); and `defines` reads
  `"Term" means ...` sentences, since a definition is published as a sentence.
  The last three were added after the first count (10 + 5 K1) and before any
  system output existed; nothing was tuned per service or per question.
* `expected_facts` are the published labels, all of them; the scorer reports a ratio.
* K0: 10 hand-written lookups per corpus in `k0-<bank>.toml`, each a 1-3 token
  fact copied from one passage; this script REFUSES a row whose fact is not on
  the line its `notes` cites.
* Dev services (Reddit, Spotify, LinkedIn) shaped the extraction prompt: they are
  their own bank and their own row, never pooled with the held-out services.
* What cannot be built is written to gold/ as absent with the reason, never padded.

    python3 make_bank.py          # writes bank-*.src.toml and gold/*.json
    python3 ../harness/attest.py --bank bank-heldout.src.toml \
        --chunks ~/.svrnmesh/indexes/ei7-recensus-fineprint --out bank-heldout.toml
"""
import json, pathlib, re, sys, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path[:0] = [str(HERE.parent / "recensus"), str(HERE.parent / "harness")]
import attest, decisions, gold_lists  # noqa: E402

SRC = HERE.parent / "spikes" / "fineprint-data" / "ota-pga-versions"
MIN_ITEMS = 5
PRIVACY, ADS, DEV = "Privacy Policy.md", "Advertising Content Policy.md", "Developer Terms.md"
# (list id, declared ontology type, document, heading pattern, table-column label, template)
TYPES = [
    ("collects", "data_type", PRIVACY, gold_lists.HEADING, None,
     "According to {s}'s privacy policy, what categories of personal data does {s} collect?"),
    ("uses", "purpose", PRIVACY, re.compile(r"how (do )?we use|why \w+ collects|purposes? for using", re.I),
     re.compile(r"purpose", re.I),
     "According to {s}'s privacy policy, for what purposes does {s} use personal data?"),
    ("shares", "recipient", PRIVACY, re.compile(r"\bshar(e|es|ed|ing)\b|disclos", re.I),
     re.compile(r"recipient", re.I),
     "According to {s}'s privacy policy, with whom does {s} share personal data?"),
    ("prohibits", "obligation", ADS, re.compile(r"^(\d+\\?\. )?prohibited (content|ads)$", re.I), None,
     "According to {s}'s advertising policy, what categories of advertising does {s} prohibit?"),
    ("defines", "defined_term", DEV, None, None,
     "According to {s}'s developer terms, what terms does the agreement define?"),
]
ITALIC = re.compile(r"^_(.+?)_")
DEFINES = re.compile(r"[“\"]\**([^”\"*]{2,60}?)\**[”\"]\**\s+(?:means|has the meaning|is defined)")
BANKS = {"heldout": ("ei7-recensus-fineprint", ["X", "Facebook", "YouTube"]),
         "dev": ("spike-fineprint-census", ["Reddit", "Spotify", "LinkedIn"])}


def table_items(md, first, end, label_ok):
    """(first-column cell, line, kind) for tables in the span whose column label fits."""
    lines, out = md.splitlines(), []
    for head, rows in decisions.tables("\n".join(lines[first:end])):
        label, displaced = decisions.column_label(head, rows, 0)
        if not any(head):                       # a layout table: emphasised cells are lead-ins
            out += [(m.group(1), first + 1, "layout-cell") for r in rows for c in r
                    if (m := ITALIC.match(c)) and m.end() == len(c)]
            continue
        if not label_ok(head, rows, label):
            continue
        for r in rows[1:] if displaced else rows:
            c = r[0] if r else ""
            n = next((k + 1 for k in range(first, end) if lines[k].lstrip().startswith("|")
                      and decisions.cell(lines[k].strip().strip("|").split("|")[0]) == c), first + 1)
            out.append((c, n, "table-cell"))
    return out


def gold(service, list_id, doc, heading, column):
    path = SRC / service / doc
    base = {"service": service, "list": list_id, "source": str(path.relative_to(HERE.parent)),
            "headings": [], "items": [], "dropped": []}
    if not path.exists():
        return {**base, "verdict": "absent", "reason": f"{service} publishes no {doc} in this corpus"}
    md = path.read_text()
    if heading is None:
        raw = [(m.group(1), md.count("\n", 0, m.start()) + 1, "definition") for m in DEFINES.finditer(md)]
    else:
        gold_lists.HEADING = heading            # the imported rule, a different pattern
        label_ok = ((lambda h, r, l: decisions.is_data_column(h, r, 0)[0]) if column is None
                    else (lambda h, r, l: bool(column.search(l))))
        raw, hs = [], gold_lists.headings(md)
        for h, first, end in gold_lists.spans(md):
            base["headings"].append({"line": h["line"], "text": h["text"], "span_ends_line": end})
            raw += gold_lists.raw_items(md, first, end) + table_items(md, first, end, label_ok)
            raw += [(x["text"], x["line"], "sub-heading") for x in hs if h["line"] < x["line"] <= end]
    seen = set()
    for text, line, kind in sorted(raw, key=lambda r: r[1]):
        text = decisions.cell(text)
        text = m.group(1) if (m := ITALIC.match(text)) else text
        label = gold_lists.strip_parentheticals(text).strip(" .:;,")
        n = decisions.norm(label)
        if not n or n in seen:
            continue
        seen.add(n)
        long = len(n.split()) > decisions.MAX_TRUTH_WORDS
        (base["dropped"] if long else base["items"]).append({"item": label, "line": line, "from": kind})
    k = len(base["items"])
    ok = k >= MIN_ITEMS
    why = (f"{k} published items, {len(base['dropped'])} prose dropped" if ok else
           f"{k} items under {len(base['headings'])} matching heading(s); a list needs {MIN_ITEMS}")
    return {**base, "verdict": "built" if ok else "absent", "reason": why}


def main():
    (HERE / "gold").mkdir(exist_ok=True)
    for bank, (corpus, services) in BANKS.items():
        rows = []
        for s in services:
            for list_id, otype, doc, heading, column, template in TYPES:
                g = gold(s, list_id, doc, heading, column)
                json.dump(g, open(HERE / "gold" / f"{s}-{list_id}.json", "w"), indent=1, ensure_ascii=False)
                print(f"{bank:<8} {s:<9} {list_id:<10} {g['verdict']:<7} {g['reason']}")
                if g["verdict"] == "built":
                    lo, hi = g["items"][0]["line"], g["items"][-1]["line"]
                    rows.append({"id": f"list-{s.lower()}-{list_id}", "category": "unattested",
                                 "question": template.format(s=s),
                                 "expected_facts": [i["item"] for i in g["items"]],
                                 "notes": f"{otype}; {g['source']} lines {lo}-{hi}; gold/{s}-{list_id}.json"})
        k0 = HERE / f"k0-{bank}.toml"
        if not k0.exists():
            print(f"{bank}: K0 never-written ({k0.name} absent) — the K1 count stopped the bank first")
        for q in tomllib.load(open(k0, "rb"))["questions"] if k0.exists() else []:
            m = re.search(r"^(.*\.md):(\d+)$", q["notes"])
            line = (SRC / m.group(1)).read_text().splitlines()[int(m.group(2)) - 1]
            assert q["expected_facts"][0].casefold() in line.casefold(), (q["id"], "fact not on the cited line")
            rows.append({"id": q["id"], "category": "unattested", **{k: q[k] for k in
                         ("question", "expected_facts", "answer1", "notes")}})
        meta = {"name": f"ontology-proof-{bank}-v1", "corpus": corpus, "description":
                f"K1 lists and K0 lookups over {', '.join(services)}, gold from each service's own "
                f"policy text (make_bank.py). {'DEVELOPMENT services: reported as their own row, never pooled with held-out.' if bank == 'dev' else 'Held-out services.'}"}
        (HERE / f"bank-{bank}.src.toml").write_text(attest.dump_bank(meta, rows))
        print(f"{bank}: {sum(r['id'].startswith('list-') for r in rows)} K1, "
              f"{sum(not r['id'].startswith('list-') for r in rows)} K0 -> bank-{bank}.src.toml")


if __name__ == "__main__":
    main()
