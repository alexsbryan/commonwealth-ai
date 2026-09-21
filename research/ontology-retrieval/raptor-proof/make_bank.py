#!/usr/bin/env python3
"""NarrativeQA rows -> one eval bank per candidate book (ei7-prove-raptor, steps 1+4).

Rerunnable: `python3 make_bank.py`. Fetches what is missing (NarrativeQA CSVs
pinned at NQA_REV, book text by the dataset's own story_url), strips the
Gutenberg header/footer, writes `books/<slug>.txt` and `bank-<slug>.toml`.

THE ONE RULE for `expected_facts`, applied to every row with no per-question
judgement: tokenize `answer1` with the eval scorer's tokenizer
(`attest.content_words`: alphanumeric runs, >=3 chars, case-folded); drop
STOPWORDS; drop words that already occur in the question (the model could echo
those for free); dedupe keeping order; keep the first 3. If that leaves
nothing, relax the question filter; if still nothing, the fact is `answer1`
verbatim (the loader refuses a row with no facts).

`category` is `k4_whole_story` on every row BY CONSTRUCTION, not by
`attest.py`'s coverage rule: NarrativeQA questions were written by annotators
who saw only the Wikipedia plot summary, never the book, which is why they
count as whole-story. Rows and answers are verbatim; `answer2` rides in `notes`.
No identity in any request: curl with a generic UA.
"""
import csv, re, subprocess, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "harness"))
from attest import K4, content_words, dump_bank  # noqa: E402  one tokenizer, one writer

NQA_REV = "904246f6d1fe99a99a08a03501fe3e619af2cee5"
NQA_RAW = f"https://raw.githubusercontent.com/google-deepmind/narrativeqa/{NQA_REV}/"
NQA_FILES = ["LICENSE", "documents.csv", "qaps.csv", "third_party/wikipedia/LICENSE",
             "third_party/wikipedia/ATTRIBUTION", "third_party/wikipedia/summaries.csv"]
# slug -> NarrativeQA document_id prefix. The six rows of candidates.tsv with pick=1.
BOOKS = {
    "pilot-and-his-wife": "e034bde5",
    "a-mans-woman": "d5a2d9f1",
    "eagle-cliff": "9e20fe26",
    "comrades": "6e7da219",
    "children-of-the-new-forest": "04d0a3d1",
    "self-control": "a2cb171c",
}
STOPWORDS = set("""the and that this these those with from into onto for was were are been being
has had have his her hers him she they them their its who whom whose what when where why how
which not but because will would could should can may might then than there here out off over
about after before while during also too very just only own one all any both each some such
himself herself themselves itself you your our does did doing get got gets goes going went
becomes became become wants wanted want make makes made take takes took""".split())


def fetch(url, dest):
    if dest.exists() and dest.stat().st_size > 0:
        print(f"cached  {dest.relative_to(HERE)}", file=sys.stderr)
        return
    print(f"fetch   {url}", file=sys.stderr)
    dest.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["curl", "-sSfL", "-m", "120", "-A", "curl/8", "-o", str(dest), url], check=True)


def strip_gutenberg(raw):
    start = re.search(r"^\*\*\* ?START OF TH.*?\*\*\*\s*$", raw, re.M)
    end = re.search(r"^\*\*\* ?END OF TH.*$", raw, re.M)
    if not (start and end):
        sys.exit("refused: no Gutenberg START/END markers — not silently keeping the licence text")
    return raw[start.end():end.start()].strip() + "\n"


def facts(question, answer1):
    words = list(dict.fromkeys(w for w in content_words(answer1) if w not in STOPWORDS))
    asked = set(content_words(question))
    return ([w for w in words if w not in asked] or words)[:3] or [answer1.strip()]


def main():
    nqa = HERE / "narrativeqa"
    for f in NQA_FILES:
        fetch(NQA_RAW + f, nqa / f)
    csv.field_size_limit(10**9)
    docs = {r["document_id"][:8]: r for r in csv.DictReader((nqa / "documents.csv").open(encoding="utf-8"))}
    qaps = list(csv.DictReader((nqa / "qaps.csv").open(encoding="utf-8")))
    for slug, short in BOOKS.items():
        doc = docs[short]
        if (doc["set"], doc["kind"]) != ("test", "gutenberg"):
            sys.exit(f"refused: {slug} is {doc['set']}/{doc['kind']}, not test/gutenberg")
        raw = HERE / "books" / "raw" / f"{slug}.txt"
        fetch(doc["story_url"], raw)
        text = strip_gutenberg(raw.read_text(encoding="utf-8-sig"))
        (HERE / "books" / f"{slug}.txt").write_text(text, encoding="utf-8")
        folded, rows, in_book = text.casefold(), [], 0
        for n, r in enumerate((q for q in qaps if q["document_id"] == doc["document_id"]), 1):
            ef = facts(r["question"], r["answer1"])
            in_book += all(w in folded for f in ef for w in content_words(f))
            rows.append({"id": f"nqa-{slug}-{n:02d}", "category": K4, "question": r["question"].strip(),
                         "expected_facts": ef, "answer1": r["answer1"].strip(),
                         "notes": f"narrativeqa {short} row {n}; answer2: {r['answer2'].strip()}"})
        meta = {"name": f"raptor-proof-{slug}-v1", "corpus": f"raptor-{slug}", "description": (
            f"All {len(rows)} NarrativeQA test-split questions for {doc['wiki_title']!r} "
            f"({doc['story_url']}), verbatim. Questions were written from the Wikipedia plot "
            f"summary, not the book, so every row is k4_whole_story by construction. "
            f"expected_facts by the one mechanical rule in make_bank.py.\n")}
        (HERE / f"bank-{slug}.toml").write_text(dump_bank(meta, rows), encoding="utf-8")
        print(f"{slug:<20} words={len(text.split()):>7} questions={len(rows):>3} "
              f"rows-with-every-fact-word-in-book={in_book}")


if __name__ == "__main__":
    main()
