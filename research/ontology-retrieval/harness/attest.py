#!/usr/bin/env python3
"""Attest a bank's gold against the corpus text, then COMPUTE each question's
kind. Nobody labels a kind by hand.

    attest.py --bank P/bank.src.toml \
              --chunks ~/.svrnmesh/indexes/chaos-secret-agent \
              --out P/bank.toml

PRE-REG-custom-ontology-and-raptor-2026-09-17, "Theory, in one paragraph and
one table":

    **Kinds are computed, never labelled by the author.** Every gold item is
    attested by exact or regex match over the corpus chunk text, not through
    retrieval, and unattested items are dropped and counted.

Two steps, in this order.

**Attestation.** Each `expected_facts` entry must appear in at least one chunk
— exact (case-insensitive substring) first, then the entry read as a regex, so
the existing OR-group form `"Winnie|Mrs Verloc|Winnie Verloc"` attests while a
malformed pattern still attests by its literal text. An entry no chunk carries
is dropped from the question and counted; a question that had facts and keeps
none is dropped whole. Chunk text is read straight off `chunks.lance` — never
through retrieval, which is the thing the study is measuring and so cannot also
be the thing that decides what is true.

**The kind.** Six rules, checked in the fixed order below; the first that holds
names the kind, and the order is derived from the pre-reg, not chosen here:

  1. `k3-tag`      — the gold's provenance is declared by whoever built it from
                     a contested pair or an original-plus-amendment, so nothing
                     the corpus says can overturn it. It goes first.
  2. `k4-arc`      — } the two halves of ONE measurement (the most any single
  3. `k0-literary` — } chunk holds of `answer1`), so their relative order can
                     never matter. They fire only where `answer1` exists, which
                     is the literary corpus — whose kinds the pre-reg's corpus
                     table gives as exactly K0 and K4.
  4. `k1-count`    — more than 12 attesting passages: past the capacity limit.
  5. `k2-bridge`   — an attesting passage that does not name the asked entity.
                     Below k1 because k1 is a count and this is an absence,
                     which a passage can satisfy incidentally.
  6. `k0-single`   — exactly one attesting passage. The control kind, last.

A rule whose input the question does not carry — no `gold_provenance`, no
`answer1`, no `entity` — is NOT judged, and the table says so per rule. It is
never read as "the rule did not hold" (ARCH principle 6: "did not answer" is
not "answered: no"). A question no rule holds for is dropped and counted.

The table on stdout is the artifact: the kept count per kind, both drop
reasons, the fact tally, what was not judged, and how many questions more than
one rule held for — without that last count the priority order decides
silently.

Exit codes: 0 = `--out` was written; 2 = a premise refusal, nothing written.

Self-test: `python3 attest.py --self-test`.
"""

import argparse
import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:                     # pragma: no cover - py<3.11
    import tomli as tomllib                     # type: ignore


class Refused(Exception):
    """A premise this script cannot supply itself. Exit 2, nothing written."""


# ── the pre-reg's constants ──────────────────────────────────────────────────
# Neither is a knob and neither is tuned here. 12 is the capacity the pre-reg
# derives from the top-20 merge (`runtime/prompts.rs:366`) against the 24,000
# character synthesis budget (`runtime/formatters.rs:39`); 0.80 is the
# fraction in the K4 and literary-K0 rules as written.
CAPACITY_PASSAGES = 12
ANSWER_COVERAGE = 0.80

# `category` values written to the output bank. The eval bank requires the key
# and copies it onto every result row (`eval_cmd/bank.rs:77-93`), so these are
# the strings the comparison groups by.
K0 = "k0_look_it_up"
K1 = "k1_list_them_all"
K2 = "k2_connect_the_dots"
K3 = "k3_what_changed"
K4 = "k4_whole_story"

# Closed set: the two provenances the pre-reg's K3 rule names. A question
# carrying anything else is a refusal, not a silent non-match.
K3_PROVENANCE = ("contested_pair", "original_amendment")


def content_words(s):
    """The tokens a fact can be scored on.

    Exactly the eval scorer's `keyword_tokens`
    (`sovereign-cli-llm/src/eval_cmd/score.rs:209`): split on non-alphanumerics,
    keep tokens of three or more characters, fold case. Reused rather than
    re-derived so "content word" means one thing across the study and the
    scorer it feeds.

    THE COST OF THAT REUSE, STATED. It keeps function words — `the`, `and`,
    `his` all clear three characters — so on a long chunk the K4 coverage
    fraction runs high for free, and the rule filing is biased toward
    literary-K0 over K4. Measured on chaos-secret-agent (316 chunks averaging
    ~2,000 characters), a 10-word `answer1` reached 80% off one chunk. A
    stopword list would tighten it and would also be a threshold this queue is
    not allowed to introduce; the pre-reg says "content words" and the scorer
    is the only definition of one this repo has. Whoever lands the literary
    corpus should read the k0-literary row of the table as the loose side.
    """
    cleaned = "".join(c if c.isalnum() else " " for c in s)
    return [t.casefold() for t in cleaned.split() if len(t) >= 3]


def _word_bounded(name):
    """Case-insensitive, word-bounded — so `Ash` does not fire inside `Ashford`.

    The same idiom as `fabricated.py:_pattern`, written out rather than
    imported: that one is private to its scorer and this file must run with no
    import of its sibling.
    """
    return re.compile(r"(?<!\w)" + re.escape(name.strip()) + r"(?!\w)", re.IGNORECASE)


# ── attestation ──────────────────────────────────────────────────────────────

def attest_fact(fact, folded_chunks, chunks):
    """Chunk indices attesting FACT — exact first, then FACT read as a regex.

    Returns `(indices, bad_regex)`. `bad_regex` is True when the entry is not a
    compilable pattern, which only costs the author a fact when exact matching
    also found nothing — reported either way rather than swallowed.
    """
    needle = fact.casefold()
    hits = [i for i, c in enumerate(folded_chunks) if needle in c]
    if hits:
        return hits, False
    try:
        rx = re.compile(fact, re.IGNORECASE)
    except re.error:
        return [], True
    return [i for i, c in enumerate(chunks) if rx.search(c)], False


class Measured:
    """What the six rules read. Computed once per question."""

    def __init__(self):
        self.facts_kept = []
        self.facts_dropped = []
        self.facts_bad_regex = []
        self.had_facts = False
        self.attesting = []          # chunk indices attesting at least one kept fact
        self.entity = None
        self.bridge_passage = None   # an attesting passage that does not name `entity`
        self.answer1 = None
        self.max_coverage = None     # most of answer1's content words any one chunk holds


def measure(question, chunks, folded_chunks):
    m = Measured()

    facts = question.get("expected_facts") or []
    m.had_facts = bool(facts)
    attesting = set()
    for fact in facts:
        hits, bad = attest_fact(fact, folded_chunks, chunks)
        if bad:
            m.facts_bad_regex.append(fact)
        if hits:
            m.facts_kept.append(fact)
            attesting.update(hits)
        else:
            m.facts_dropped.append(fact)
    m.attesting = sorted(attesting)

    entity = (question.get("entity") or "").strip()
    if entity:
        m.entity = entity
        pattern = _word_bounded(entity)
        for i in m.attesting:
            if not pattern.search(chunks[i]):
                m.bridge_passage = i
                break

    answer1 = (question.get("answer1") or "").strip()
    if answer1:
        m.answer1 = answer1
        want = set(content_words(answer1))
        # An `answer1` of nothing but short words leaves the rule nothing to
        # measure; that reads as not-judged, not as 0% coverage.
        if want:
            m.max_coverage = max(
                (len(want & set(content_words(c))) / len(want) for c in chunks),
                default=0.0,
            )
    return m


# ── the six rules, in the order the module docstring fixes ───────────────────

def _k3_tag(_m, provenance):
    if provenance is None:
        return None
    return provenance in K3_PROVENANCE


def _k4_arc(m, _p):
    if m.max_coverage is None:
        return None
    return m.max_coverage < ANSWER_COVERAGE


def _k0_literary(m, _p):
    if m.max_coverage is None:
        return None
    return m.max_coverage >= ANSWER_COVERAGE


def _k1_count(m, _p):
    return len(m.attesting) > CAPACITY_PASSAGES


def _k2_bridge(m, _p):
    if m.entity is None:
        return None
    return m.bridge_passage is not None


def _k0_single(m, _p):
    return len(m.attesting) == 1


# (rule id, kind written to `category`, rule as the table prints it,
#  the question key whose absence leaves the rule unjudged, predicate)
RULES = (
    ("k3-tag", K3, "gold tagged contested_pair | original_amendment",
     "gold_provenance", _k3_tag),
    ("k4-arc", K4, "no chunk holds >=80% of answer1 content words",
     "answer1", _k4_arc),
    ("k0-literary", K0, "one chunk holds >=80% of answer1 content words",
     "answer1", _k0_literary),
    ("k1-count", K1, "more than 12 attesting passages",
     None, _k1_count),
    ("k2-bridge", K2, "an attesting passage that does not name `entity`",
     "entity", _k2_bridge),
    ("k0-single", K0, "exactly one attesting passage",
     None, _k0_single),
)


class Report:
    def __init__(self, n_questions, n_chunks):
        self.n_questions = n_questions
        self.n_chunks = n_chunks
        self.by_rule = {r[0]: 0 for r in RULES}
        self.unjudged = {r[0]: 0 for r in RULES}
        self.multi_rule = 0
        self.dropped_no_rule = 0
        self.dropped_no_fact = 0
        self.facts_total = 0
        self.facts_kept = 0
        self.facts_bad_regex = 0


def attest_bank(bank_questions, chunks):
    """-> (kept questions with computed `category`, Report).

    Neither argument is read from disk: the rules are exercised in memory by
    `--self-test`, and `main` supplies the same two lists from `--bank` and
    `--chunks`.
    """
    folded = [c.casefold() for c in chunks]
    report = Report(len(bank_questions), len(chunks))
    kept = []

    for q in bank_questions:
        qid = q.get("id", "(no id)")
        provenance = q.get("gold_provenance")
        if provenance is not None and provenance not in K3_PROVENANCE:
            raise Refused(
                f"question {qid}: gold_provenance={provenance!r} is not one of "
                f"{' | '.join(K3_PROVENANCE)}")

        m = measure(q, chunks, folded)
        report.facts_total += len(m.facts_kept) + len(m.facts_dropped)
        report.facts_kept += len(m.facts_kept)
        report.facts_bad_regex += len(m.facts_bad_regex)

        if m.had_facts and not m.facts_kept:
            report.dropped_no_fact += 1
            print(f"  drop {qid}: every expected_fact unattested", file=sys.stderr)
            continue

        held = []
        for rule_id, kind, _text, _needs, predicate in RULES:
            verdict = predicate(m, provenance)
            if verdict is None:
                report.unjudged[rule_id] += 1
            elif verdict:
                held.append((rule_id, kind))

        if not held:
            report.dropped_no_rule += 1
            print(f"  drop {qid}: no rule held "
                  f"({len(m.attesting)} attesting passages)", file=sys.stderr)
            continue

        rule_id, kind = held[0]
        if len(held) > 1:
            report.multi_rule += 1
        report.by_rule[rule_id] += 1
        print(f"  {qid}: {kind} by {rule_id} "
              f"({len(m.attesting)} attesting passages"
              + (f", also {', '.join(r for r, _ in held[1:])}" if len(held) > 1 else "")
              + ")", file=sys.stderr)

        out = dict(q)
        out["category"] = kind
        if m.had_facts:
            out["expected_facts"] = m.facts_kept
        kept.append(out)

    return kept, report


def format_table(report, kept):
    lines = []
    lines.append(f"attest: {report.n_questions} questions against "
                 f"{report.n_chunks} chunks")
    lines.append("")
    lines.append(f"  {'kind':<22} {'rule':<48} {'kept':>5}")
    for rule_id, kind, text, _needs, _p in RULES:
        lines.append(f"  {kind:<22} {text:<48} {report.by_rule[rule_id]:>5}")
    lines.append("")
    lines.append(f"  kept {len(kept)} of {report.n_questions} questions")
    lines.append(f"  dropped {report.dropped_no_rule:>3} — no rule held")
    lines.append(f"  dropped {report.dropped_no_fact:>3} — every expected_fact unattested")
    lines.append(f"  facts   {report.facts_kept} of {report.facts_total} attested; "
                 f"{report.facts_total - report.facts_kept} dropped "
                 f"({report.facts_bad_regex} malformed as a regex)")
    unjudged = " · ".join(
        f"{rule_id} {report.unjudged[rule_id]} (no `{needs}`)"
        for rule_id, _k, _t, needs, _p in RULES
        if needs and report.unjudged[rule_id]
    )
    lines.append(f"  not judged: {unjudged or 'none'}")
    lines.append(f"  more than one rule held: {report.multi_rule} "
                 f"(assigned by the order above)")
    return "\n".join(lines)


# ── I/O ──────────────────────────────────────────────────────────────────────

def load_chunks(source):
    """Chunk TEXT off the index, never through retrieval.

    SOURCE is an installed index directory or the `chunks.lance` inside it.
    """
    path = Path(source).expanduser()
    if path.name != "chunks.lance":
        path = path / "chunks.lance"
    if not path.is_dir():
        raise Refused(f"no chunks.lance at {path} — --chunks wants an installed "
                      f"index directory")
    try:
        import lance
    except ModuleNotFoundError:
        raise Refused("python `lance` is not importable; chunk text cannot be "
                      "read (pip install pylance)")
    table = lance.dataset(str(path)).to_table(columns=["content"])
    return [c or "" for c in table.column("content").to_pylist()]


def _toml_value(value, where):
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int) or isinstance(value, float):
        return repr(value)
    if isinstance(value, str):
        escaped = (value.replace("\\", "\\\\").replace('"', '\\"')
                   .replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t"))
        return f'"{escaped}"'
    if isinstance(value, list):
        if all(isinstance(v, str) for v in value):
            return "[" + ", ".join(_toml_value(v, where) for v in value) + "]"
    raise Refused(f"{where}: this writer cannot render {type(value).__name__} — "
                  f"refusing rather than dropping the key")


def dump_bank(meta, questions):
    """The bank back as TOML, key order preserved.

    A value shape this writer cannot render is a refusal, never a dropped key:
    a bank that silently lost its latency budget would still load and still
    look right.
    """
    out = ["[bank]"]
    for key, value in meta.items():
        out.append(f"{key} = {_toml_value(value, f'[bank].{key}')}")
    for q in questions:
        qid = q.get("id", "?")
        out.append("")
        out.append("[[questions]]")
        for key, value in q.items():
            out.append(f"{key} = {_toml_value(value, f'{qid}.{key}')}")
    return "\n".join(out) + "\n"


def run(args):
    bank_path = Path(args.bank)
    with bank_path.open("rb") as fh:
        doc = tomllib.load(fh)
    if "bank" not in doc:
        hint = (" (this looks like the chaos-monkey `[meta]` form, which the "
                "eval bank does not load)") if "meta" in doc else ""
        raise Refused(f"{bank_path}: no [bank] table{hint}")
    questions = doc.get("questions") or []
    if not questions:
        raise Refused(f"{bank_path}: no [[questions]]")

    chunks = load_chunks(args.chunks)
    if not chunks:
        raise Refused(f"{args.chunks}: chunks.lance holds no rows")

    kept, report = attest_bank(questions, chunks)
    table = format_table(report, kept)
    print(table)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(dump_bank(doc["bank"], kept), encoding="utf-8")
    print(f"\nwrote {out_path} — {len(kept)} questions")
    return 0


# ── self-test ────────────────────────────────────────────────────────────────

def _corpus():
    """An in-memory corpus, small enough to reason about by eye.

    12 congress chunks — exactly the capacity limit — and a 13th that says
    "delegate" without saying "attended the congress", so one fact clears the
    limit and a second sits on it. Plus the handful the other rules key on.
    """
    delegates = ["Karl Yundt", "Michaelis", "Ossipon", "the Professor",
                 "Toodles", "Chief Inspector Heat", "the Assistant Commissioner",
                 "Mr Vladimir", "Winnie", "Stevie", "Mrs Neale", "Ethelred"]
    chunks = [f"{name} attended the congress as a delegate." for name in delegates]
    chunks += [
        "The shop stood at 32 Brett Street, its window full of photographs.",
        "Mr Vladimir named the Greenwich Observatory as the target.",
        "The bomb went off early and the Greenwich Observatory stood untouched.",
        "Section 4.2 of the agreement fixed the rate at five per cent.",
        "The lady patroness received F. P. (Future of the Proletariat pamphlets.",
        "Mrs Verloc sat at the counter and said nothing at all.",
        "Stevie walked the streets of London drawing circles on paper, and the "
        "cab horse plodded on through the mud of the evening.",
        "A congress delegate spoke of the circles Stevie drew.",
        "The evening closed over London.",
    ]
    return chunks


def self_test():
    """One question per rule, one that fails its rule, and the attestation and
    writer branches each with a named failing input.

    Every case carries a PLANT: the input an obvious-but-wrong implementation
    gets wrong. A case that could not be set up reads `could-not-judge`, one
    whose fixture is absent on this host reads `never-ran` — never `passed`.
    """
    results = []
    chunks = _corpus()

    def case(name, fn):
        try:
            verdict, detail = fn()
        except Exception as e:                   # noqa: BLE001 - a broken fixture
            results.append((name, "could-not-judge", f"{type(e).__name__}: {e}"))
            return
        results.append((name, verdict, detail))

    def kind_of(question):
        kept, report = attest_bank([question], chunks)
        if not kept:
            return None, report
        return kept[0]["category"], report

    def k1_more_than_twelve_passages():
        # PLANT: `>= 12` instead of `> 12` puts a question at exactly the
        # capacity limit into K1, where the pre-reg's prediction is that bare
        # RAG still fits it.
        many, _ = kind_of({"id": "q-k1", "question": "Who attended?",
                           "expected_facts": ["delegate"]})
        at_limit, _ = kind_of({"id": "q-12", "question": "Who attended?",
                               "expected_facts": ["attended the congress"]})
        # 13 chunks carry "delegate"; exactly 12 carry the longer phrase.
        return ("passed" if many == K1 and at_limit != K1 else "failed",
                f"13 passages -> {many}; 12 passages -> {at_limit}")

    def k0_single_passage_and_an_unattested_fact_is_dropped():
        # PLANT: keeping the unattested fact leaves gold in the bank that no
        # arm can ever score, so every arm loses the same point and the kind
        # count is right for the wrong reason.
        kept, report = attest_bank([{
            "id": "q-k0", "question": "Where was the shop?",
            "expected_facts": ["Brett Street", "Threadneedle Street"],
        }], chunks)
        ok = (kept and kept[0]["category"] == K0
              and kept[0]["expected_facts"] == ["Brett Street"]
              and report.facts_kept == 1 and report.facts_total == 2)
        return ("passed" if ok else "failed",
                f"-> {kept[0]['category'] if kept else None}, "
                f"facts {kept[0]['expected_facts'] if kept else None}")

    def k2_a_passage_that_does_not_name_the_entity():
        # PLANT: two attesting passages is neither 1 nor >12, so without the
        # bridge rule this question is DROPPED — the exact shape the study
        # exists to measure would never reach an arm.
        kind, _ = kind_of({"id": "q-k2", "question": "What was the target?",
                           "entity": "Mr Vladimir",
                           "expected_facts": ["Greenwich Observatory"]})
        return ("passed" if kind == K2 else "failed", f"-> {kind}")

    def k3_provenance_outranks_the_counts():
        # PLANT: one attesting passage. Check the counts before the tag and
        # this contested-pair question is filed as K0 "look it up", which is
        # the control kind — the tie the study predicts.
        kind, _ = kind_of({"id": "q-k3", "question": "What rate applies?",
                           "gold_provenance": "original_amendment",
                           "expected_facts": ["Section 4.2"]})
        return ("passed" if kind == K3 else "failed", f"-> {kind}")

    def k4_no_chunk_holds_the_answer():
        # PLANT: one attesting passage again. Without the answer1 pair ranking
        # above the counts, a whole-story question is filed K0 and RAPTOR is
        # measured on a lookup.
        kind, _ = kind_of({
            "id": "q-k4", "question": "What is the arc?",
            "answer1": "Stevie carries the bomb to Greenwich and dies, and "
                       "Winnie kills Verloc before drowning in the Channel.",
            "expected_facts": ["Section 4.2"]})
        return ("passed" if kind == K4 else "failed", f"-> {kind}")

    def k0_literary_one_chunk_holds_the_answer():
        # PLANT: three attesting passages — neither 1 nor >12 — so the count
        # rules drop this question. The literary reading is what keeps it, and
        # it must land on K0, not K4.
        kind, _ = kind_of({
            "id": "q-k0-lit", "question": "What did Stevie do?",
            "answer1": "Stevie walked the streets drawing circles on paper.",
            "expected_facts": ["Stevie"]})
        return ("passed" if kind == K0 else "failed", f"-> {kind}")

    def a_question_no_rule_holds_for_is_dropped():
        # PLANT: assigning a default kind here is the failure this whole file
        # exists to prevent — an unclassifiable question filed as K0 pads the
        # control arm with questions nothing attested.
        kept, report = attest_bank([{
            "id": "q-none", "question": "What happened in the evening?",
            "expected_facts": ["evening"]}], chunks)
        ok = kept == [] and report.dropped_no_rule == 1
        return ("passed" if ok else "failed",
                f"2 attesting passages, no entity/answer1 -> {kept}")

    def an_or_group_attests_as_a_regex():
        # PLANT: exact matching alone drops the OR-group form the existing
        # chaos-monkey bank is written in (`secret_agent.toml:27`), so a bank
        # converted from it would lose most of its gold silently.
        hits, bad = attest_fact("Winnie|Mrs Verloc|Winnie Verloc",
                                [c.casefold() for c in chunks], chunks)
        return ("passed" if hits and not bad else "failed",
                f"-> {len(hits)} passages, bad_regex={bad}")

    def a_malformed_pattern_still_attests_by_its_literal_text():
        # PLANT: compiling the fact as a regex FIRST throws on this entry (an
        # unclosed group), and a bare try/except around it drops a fact the
        # corpus plainly carries.
        fact = "F. P. (Future of the Proletariat"
        try:
            re.compile(fact)
            return "failed", "the fixture compiles as a regex — the plant is gone"
        except re.error:
            pass
        hits, bad = attest_fact(fact, [c.casefold() for c in chunks], chunks)
        return ("passed" if len(hits) == 1 and not bad else "failed",
                f"-> {len(hits)} passages, bad_regex={bad}")

    def an_out_of_set_provenance_is_refused():
        # PLANT: treating an unknown tag as "not K3" files a question the
        # author explicitly marked as contested under a computed kind, and the
        # typo is never seen.
        try:
            attest_bank([{"id": "q-bad", "question": "?",
                          "gold_provenance": "amended", "expected_facts": ["Stevie"]}],
                        chunks)
        except Refused as e:
            return ("passed" if "q-bad" in str(e) else "failed", str(e))
        return "failed", "accepted an out-of-set gold_provenance"

    def the_writer_refuses_a_shape_it_cannot_render():
        # PLANT: falling through to str() renders a nested table as a quoted
        # string, so the bank still parses and the latency budget is gone.
        try:
            dump_bank({"name": "b", "corpus": "c", "latency_budget": {"p95": 1}}, [])
        except Refused as e:
            return ("passed" if "latency_budget" in str(e) else "failed", str(e))
        return "failed", "rendered a nested table"

    def chunk_text_is_readable_off_an_installed_index():
        # The one case that touches this host. Absent index reads `never-ran`:
        # the check is owed, not passed (ARCH principle 5).
        index = Path("~/.svrnmesh/indexes/chaos-secret-agent").expanduser()
        if not (index / "chunks.lance").is_dir():
            return "never-ran", f"no index at {index}"
        got = load_chunks(index)
        return ("passed" if got and any(c.strip() for c in got) else "failed",
                f"{len(got)} chunks, first {len(got[0])} chars")

    case("k1-more-than-twelve", k1_more_than_twelve_passages)
    case("k0-single-passage", k0_single_passage_and_an_unattested_fact_is_dropped)
    case("k2-bridge-passage", k2_a_passage_that_does_not_name_the_entity)
    case("k3-provenance-outranks", k3_provenance_outranks_the_counts)
    case("k4-no-chunk-holds-it", k4_no_chunk_holds_the_answer)
    case("k0-literary-one-chunk", k0_literary_one_chunk_holds_the_answer)
    case("no-rule-holds-dropped", a_question_no_rule_holds_for_is_dropped)
    case("or-group-attests", an_or_group_attests_as_a_regex)
    case("malformed-pattern-attests", a_malformed_pattern_still_attests_by_its_literal_text)
    case("out-of-set-provenance", an_out_of_set_provenance_is_refused)
    case("writer-refuses-shape", the_writer_refuses_a_shape_it_cannot_render)
    case("chunks-readable", chunk_text_is_readable_off_an_installed_index)

    print("", file=sys.stderr)
    for name, verdict, detail in results:
        print(f"  {name:<30} {verdict:<16} {detail}", file=sys.stderr)
    bad = [n for n, v, _ in results if v not in ("passed", "never-ran")]
    never = [n for n, v, _ in results if v == "never-ran"]
    tail = f", {len(never)} never-ran" if never else ""
    print(f"self-test: {len(results) - len(bad) - len(never)}/{len(results)} "
          f"passed{tail}", file=sys.stderr)
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bank", help="bank TOML whose categories are computed")
    ap.add_argument("--chunks", help="installed index dir, or its chunks.lance")
    ap.add_argument("--out", help="bank TOML to write")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if not (args.bank and args.chunks and args.out):
        ap.error("--bank, --chunks and --out are all required")
    try:
        return run(args)
    except Refused as e:
        print(f"attest: refused — {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
