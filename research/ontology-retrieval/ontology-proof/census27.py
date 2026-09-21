#!/usr/bin/env python3
"""How many CLEAN scattered-list K1 cells do all 27 services yield? Zero model cost.

THE RULE, written before its yield was looked at. Source: `<Service>/Privacy
Policy.md` only. Types: collects / uses / shares only (defines and prohibits are
single-section lookups and are out). Items come from `make_bank.gold` unchanged
(gold_lists' span rule + table column + italic lead-in + sub-heading, word cap
`decisions.MAX_TRUTH_WORDS`, de-duplicated on `decisions.norm`). Then, by SHAPE:

  link-only   the item's source line, less its list marker / heading hashes /
              emphasis, is one `[text](url)` link (list items, lead-ins, headings)
  imperative  the normalised item's first word is in IMPERATIVE, a closed set
              fixed here and not tuned: a call to action, not a list member
  numbered    the item reads as numbered-heading text, NUMBERED
  one-word    one word after normalisation
  dangling    the cleaned text ends in `;` or in `; or` / `, or` / `; and` / `, and`
  prose       (already applied upstream) over the word cap

A (service, type) cell QUALIFIES with 5..40 surviving items; over 40 is a
scraped navigation tree, not a list. Every drop is written with its line and
the rule that dropped it to census27/<Service>-<list>.json. No bank is written.

    python3 census27.py        # table, totals, seeded sample -> census27/
"""
import json, pathlib, random, re, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import make_bank  # noqa: E402
from make_bank import decisions, gold_lists  # noqa: E402

IMPERATIVE = {"learn", "see", "view", "manage", "take", "contact", "create", "change",
              "visit", "read", "go", "find", "download"}
NUMBERED = re.compile(r"^\d+(\\?\.\d+)*\\?\.?\s")
DANGLING = re.compile(r"(;|[;,]\s*(or|and))\s*$", re.I)
LINK_ONLY = re.compile(r"^\[[^\]]*\]\([^)]*\)[.:]?$")
MARKER = re.compile(r"^\s*(?:[*+-]|\d+[.)]|#{1,6})\s+")
LO, HI, SEED = 5, 40, 27
EXTRACTED = {"X", "Facebook", "YouTube", "Reddit", "Spotify", "LinkedIn"}
TYPES = [t for t in make_bank.TYPES if t[0] in ("collects", "uses", "shares")]


def shape(item, lines):
    """The rule that drops ITEM, or None."""
    n = decisions.norm(item["item"]).split()
    src = lines[item["line"] - 1] if item["line"] <= len(lines) else ""
    bare = MARKER.sub("", src).strip().strip("*_ ")
    if item["from"] in ("list-item", "bold-lead-in", "sub-heading") and LINK_ONLY.match(bare):
        return "link-only"
    if NUMBERED.match(item["item"]):
        return "numbered"
    if len(n) == 1:
        return "one-word"
    if n[0] in IMPERATIVE:
        return "imperative"
    if DANGLING.search(decisions.cell(bare)) and item["from"] != "table-cell":
        return "dangling"
    return None


def est_chunks(md, max_chars=2000):
    """Greedy paragraph packing to the recipe's max_chars — an ESTIMATE of the
    `paragraph` chunker, calibrated in main() against an installed corpus."""
    n, size = 0, 0
    for p in (p for p in re.split(r"\n\s*\n", md) if p.strip()):
        if size and size + len(p) > max_chars:
            n, size = n + 1, 0
        size += len(p) + 2
        while size > max_chars:
            n, size = n + 1, size - max_chars
    return n + bool(size)


def main():
    out = HERE / "census27"
    out.mkdir(exist_ok=True)
    services = sorted(p.name for p in make_bank.SRC.iterdir() if p.is_dir() and not p.name.startswith("."))
    cells, words_todo = {}, 0
    print(f"{'service':<12} {'collects':>9} {'uses':>9} {'shares':>9}   names-self  words  headings  est-chunks")
    for s in services:
        path = make_bank.SRC / s / make_bank.PRIVACY
        md = path.read_text() if path.exists() else ""
        lines, row = md.splitlines(), []
        for list_id, _otype, doc, heading, column, _tpl in TYPES:
            g = make_bank.gold(s, list_id, doc, heading, column)
            kept, dropped = [], [{**d, "rule": "prose"} for d in g["dropped"]]
            for i in g["items"]:
                rule = shape(i, lines)
                (dropped.append({**i, "rule": rule}) if rule else kept.append(i))
            k = len(kept)
            verdict = ("no-policy" if not md else "no-heading" if not g["headings"] else
                       "qualifies" if LO <= k <= HI else f"under-{LO}" if k < LO else f"over-{HI}")
            json.dump({"service": s, "list": list_id, "source": g["source"], "verdict": verdict,
                       "headings": g["headings"], "items": kept, "dropped": dropped},
                      open(out / f"{s}-{list_id}.json", "w"), indent=1, ensure_ascii=False)
            if verdict == "qualifies":
                cells[(s, list_id)] = kept
            row.append(f"{k}" if verdict == "qualifies" else f"{verdict}({k})")
        named = len(re.findall(rf"(?<!\w){re.escape(s)}(?!\w)", md))
        words = len(md.split())
        words_todo += 0 if s in EXTRACTED else words
        print(f"{s:<12} {row[0]:>9} {row[1]:>9} {row[2]:>9}   {named:>10}  {words:>5}  "
              f"{len(gold_lists.headings(md)):>8}  {est_chunks(md):>10}")
    done = sum(s in EXTRACTED for s, _ in cells)
    print(f"\nqualifying cells: {len(cells)}  (6 extracted services: {done}; 21 not extracted: {len(cells) - done})")
    print(f"words in the 21 un-extracted Privacy Policy files: {words_todo}")
    cal = HERE.parent / "recensus" / "fineprint.md"
    if cal.exists():
        print(f"calibration: est_chunks(recensus/fineprint.md) = {est_chunks(cal.read_text())} "
              f"(the installed ei7-recensus-fineprint index holds 1870 chunks)")
    rng = random.Random(SEED)
    for key in rng.sample(sorted(cells), min(5, len(cells))):
        items = cells[key]
        print(f"sample {key[0]}-{key[1]} ({len(items)}):",
              [i["item"] for i in rng.sample(items, min(5, len(items)))])


if __name__ == "__main__":
    main()
