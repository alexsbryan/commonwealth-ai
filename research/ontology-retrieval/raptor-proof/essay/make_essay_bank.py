#!/usr/bin/env python3
"""templates.toml + NarrativeQA's held-out text -> essay/bank-<slug>.toml.

    python3 make_essay_bank.py <slug>        # a key of make_bank.BOOKS

THE SLOT RULE, applied mechanically, never by reading the book or an answer.
The pool is external text only: the held-out Wikipedia plot summary plus this
book's NarrativeQA questions and both reference answers.
  name        a capitalised token of >= 3 letters (possessive 's stripped) that
              occurs at least once in the pool NOT at the start of a sentence,
              is not a word of the title and is not in HONORIFICS.
  protagonist the name with the most occurrences in the pool.
  second      the next. Ties go to whichever appears first in the summary.
  title       documents.csv `wiki_title`, a trailing "(...)" removed.
The rule counts mentions; it does not know who a story is "about". What it
picked is printed and written into the bank header so a reader can disagree.

`expected_facts` are a FORMALITY here (the loader refuses a row without them;
essay_judge.py does the scoring): the summary's names by frequency, at most 8,
case-folded, minus any the question already contains. `answer1` is the held-out
summary verbatim on every row. `category` is k4_whole_story, as make_bank.py.
Fetching is make_bank.py's own path (pinned NQA_REV, generic UA, no identity).
"""
import csv, re, sys, tomllib
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent.parent / "harness"))
from make_bank import BOOKS, NQA_FILES, NQA_RAW, fetch  # noqa: E402  one fetch path, one book list
from attest import K4, dump_bank  # noqa: E402  one writer

HONORIFICS = set("mr mrs miss ms dr sir lady lord col colonel captain capt major general "
                 "admiral king queen prince princess father mother aunt uncle saint".split())
MAX_FACTS = 8
TOKEN = re.compile(r"[A-Za-z][A-Za-z'’-]*")


def names(pool, title):
    """Counter of names in `pool` by THE SLOT RULE; insertion order = first seen."""
    title_words = {w.casefold() for w in re.findall(r"[A-Za-z]+", title)}
    seen, mid = Counter(), set()
    for m in TOKEN.finditer(pool):
        tok = re.sub(r"['’]s$", "", m.group()).strip("'’-")
        if len(tok) < 3 or not tok[0].isupper() or tok.isupper():
            continue
        if tok.casefold() in title_words or tok.casefold() in HONORIFICS:
            continue
        seen[tok] += 1
        before = pool[:m.start()].rstrip(" \t\"'“”‘’(")
        if before and before[-1] not in ".!?\n:":
            mid.add(tok)
    return Counter({n: c for n, c in seen.items() if n in mid})


def ranked(counter, order_text):
    first = {n: (order_text.find(n) if n in order_text else 10**9) for n in counter}
    return sorted(counter, key=lambda n: (-counter[n], first[n]))


def main(argv):
    if len(argv) != 1 or argv[0] not in BOOKS:
        sys.exit(f"usage: make_essay_bank.py <slug>   one of: {', '.join(BOOKS)}")
    slug, short = argv[0], BOOKS[argv[0]]
    nqa = HERE.parent / "narrativeqa"
    for f in NQA_FILES:
        fetch(NQA_RAW + f, nqa / f)
    csv.field_size_limit(10**9)
    doc = next(r for r in csv.DictReader((nqa / "documents.csv").open(encoding="utf-8"))
               if r["document_id"].startswith(short))
    summary = next((r["summary"].strip() for r in csv.DictReader(
        (nqa / "third_party/wikipedia/summaries.csv").open(encoding="utf-8"))
        if r["document_id"] == doc["document_id"]), "")
    if not summary:
        sys.exit(f"refused: no held-out summary for {slug}; there is no reference to judge against")
    qa = [r for r in csv.DictReader((nqa / "qaps.csv").open(encoding="utf-8"))
          if r["document_id"] == doc["document_id"]]
    title = re.sub(r"\s*\([^)]*\)\s*$", "", doc["wiki_title"]).strip()
    pool = summary + "\n" + "\n".join(f"{r['question']}\n{r['answer1']}\n{r['answer2']}" for r in qa)
    order = ranked(names(pool, title), summary)
    if len(order) < 2:
        sys.exit(f"refused: the slot rule found {len(order)} name(s) in the pool; two are needed")
    slots = {"<title>": title, "<protagonist>": order[0], "<second_character>": order[1]}
    in_summary = names(summary + "\n", title)
    fact_names = [n.casefold() for n in ranked(in_summary, summary)][:MAX_FACTS] or [order[0].casefold()]

    templates = tomllib.loads((HERE / "templates.toml").read_text(encoding="utf-8"))["template"]
    rows = []
    for t in templates:   # the row id is the template id: reordering templates.toml renames nothing
        q = t["question"]
        for slot, value in slots.items():
            q = q.replace(slot, value)
        if re.search(r"<\w+>", q) or not q.rstrip().endswith("?"):
            sys.exit(f"refused: template {t['id']} left a slot unfilled or is not a question: {q!r}")
        facts = [f for f in fact_names if f not in q.casefold()] or fact_names
        rows.append({"id": f"essay-{slug}-{t['id']}", "category": K4, "question": q,
                     "expected_facts": facts, "answer1": summary,
                     "notes": f"essay template `{t['id']}`: {t['aspect']}"})
    sentences = len([s for s in re.split(r"(?<=[.!?])\s+", summary) if s.strip()])
    pool_counts = names(pool, title)
    picked = ", ".join(f"{n}={pool_counts[n]}" for n in order[:4])
    meta = {"name": f"raptor-proof-essay-{slug}-v1", "corpus": f"raptor-{slug}", "description": (
        f"{len(rows)} essay-level questions for {title!r} from essay/templates.toml, slots filled by "
        f"the mechanical rule in essay/make_essay_bank.py from NarrativeQA's held-out text only "
        f"(name mentions in summary+questions: {picked}; protagonist={order[0]}, "
        f"second_character={order[1]}). answer1 is the held-out Wikipedia plot summary "
        f"({len(summary.split())} words, {sentences} sentences), the same on every row. "
        f"expected_facts are a loader formality (the summary's names); essay/essay_judge.py scores.\n")}
    out = HERE / f"bank-{slug}.toml"
    out.write_text(dump_bank(meta, rows), encoding="utf-8")
    tomllib.loads(out.read_text(encoding="utf-8"))  # the writer's output must load
    print(f"{slug}: title={title!r} protagonist={order[0]} second={order[1]} ({picked}) "
          f"summary={len(summary.split())}w/{sentences}s facts={fact_names} -> {out.relative_to(HERE.parent)}")
    for r in rows:
        print(f"  {r['id']}: {r['question']}")


if __name__ == "__main__":
    main(sys.argv[1:])
