#!/usr/bin/env python3
"""Fabricated members — the deterministic half of bar 3, scored off the answer
text alone.

PRE-REG-custom-ontology-and-raptor-2026-09-17, "Metrics":

    **Fabricated members** (new, deterministic): names from the corpus's truth
    vocabulary that the answer states as members but that are not gold. Names
    outside the vocabulary are not counted, and that limit is stated.

THE LIMIT, STATED. A name that is in no corpus vocabulary — one the model
invented outright, or a real person it imported from its weights — is NEVER
counted here, and this scorer cannot see it. It measures one failure mode: the
answer reaching into the corpus and naming a member the gold set does not
carry. An answer that invents "Colonel Sandoval" out of nothing scores 0. That
is the metric's floor, not a bug to patch: the pre-reg's fine-print corpus says
the same thing from the other side — where the truth set is incomplete
(crowd-sourced ToS;DR), the vocabulary cannot settle a non-gold name either and
two annotators read the policy text instead.

Matching, in the three properties the count depends on:

  - case-insensitive, so "marcus vale" in a lowercase answer counts;
  - word-bounded, so "Ash" does not fire inside "Ashford";
  - longest name first, with each matched span consumed, so an answer stating
    the gold "Alexander III" does not ALSO count the vocabulary's "Alexander".

Name matching is not reused from `sovereign/bench/chaos_monkey/
fabrication_etiology.py`: that file's `names_in` is a proper-noun regex with no
vocabulary (the opposite problem — it finds the names nobody declared), and its
`occurrences` is a case-sensitive `str.find` with no word boundary, which is
precisely the substring trap the third property above exists to avoid.

Self-test: `python3 fabricated.py --self-test`.
"""

import argparse
import re
import sys


def _key(name):
    """The identity a name is compared under — case-folded, edges trimmed."""
    return name.strip().casefold()


def _pattern(name):
    return re.compile(r"(?<!\w)" + re.escape(name.strip()) + r"(?!\w)", re.IGNORECASE)


def fabricated_members(answer: str, vocabulary: list[str], gold: list[str]) -> list[str]:
    """Vocabulary names the ANSWER states that are not in GOLD.

    Returns each such name once, in the order it first appears in the answer.
    A name outside VOCABULARY is never counted — see the module docstring for
    that limit, which the pre-reg states and this function does not exceed.
    """
    gold_keys = {_key(g) for g in gold}
    taken = []            # spans claimed by an earlier (longer) name
    found = []            # (offset, name) for the names that count
    seen = set()

    for name in sorted({v for v in vocabulary if v.strip()}, key=lambda n: (-len(n.strip()), n)):
        for m in _pattern(name).finditer(answer):
            if any(start < m.end() and m.start() < end for start, end in taken):
                continue
            taken.append((m.start(), m.end()))
            key = _key(name)
            if key in gold_keys or key in seen:
                continue
            seen.add(key)
            found.append((m.start(), name))

    return [name for _, name in sorted(found)]


# ── self-test ────────────────────────────────────────────────────────────────

def self_test():
    """The four failing inputs the scorer is named for.

    Each case carries a PLANT: an input the obvious-but-wrong implementation
    gets wrong. A case that could not be set up reads `could-not-judge`, never
    `passed`.
    """
    results = []

    def case(name, fn):
        try:
            ok, detail = fn()
        except Exception as e:                     # noqa: BLE001 — a broken fixture
            results.append((name, "could-not-judge", f"{type(e).__name__}: {e}"))
            return
        results.append((name, "passed" if ok else "failed", detail))

    VOCAB = ["Marcus Vale", "Ines Roth", "Alexander", "Alexander III", "Ash"]

    def a_gold_name_is_not_counted():
        # PLANT: counting every vocabulary hit makes the CORRECT answer score
        # worst — the arm that names the most gold members looks the most
        # fabricating.
        out = fabricated_members("The handler was Marcus Vale.", VOCAB, ["Marcus Vale"])
        return out == [], f"gold-only answer -> {out}"

    def a_non_gold_vocabulary_name_is_counted_once():
        # PLANT: appending per occurrence turns one fabricated member repeated
        # three times into three, so verbosity alone moves the bar.
        answer = ("Ines Roth met Marcus Vale. Ines Roth then left, and "
                  "ines roth was never seen again.")
        out = fabricated_members(answer, VOCAB, ["Marcus Vale"])
        return out == ["Ines Roth"], f"3 mentions (one lowercased) -> {out}"

    def an_out_of_vocabulary_name_is_not_counted():
        # PLANT: a proper-noun detector (fabrication_etiology.names_in) flags
        # "Colonel Sandoval" here. This scorer must not — that is the pre-reg's
        # stated limit, and reporting it would be a number the vocabulary
        # cannot support.
        out = fabricated_members(
            "Marcus Vale reported to Colonel Sandoval.", VOCAB, ["Marcus Vale"])
        return out == [], f"invented name -> {out}"

    def a_substring_of_a_stated_name_is_not_counted():
        # PLANT: two ways to read the same trap. Without longest-first span
        # consumption, the gold "Alexander III" also fires the vocabulary's
        # "Alexander"; without a word boundary, "Ash" fires inside "Ashford".
        # Either one scores a fabrication against an answer that stated only
        # the gold member.
        out = fabricated_members(
            "Alexander III signed the order at Ashford.", VOCAB, ["Alexander III"])
        return out == [], f"gold 'Alexander III' at 'Ashford' -> {out}"

    case("gold-name-not-counted", a_gold_name_is_not_counted)
    case("non-gold-counted-once", a_non_gold_vocabulary_name_is_counted_once)
    case("out-of-vocabulary-not-counted", an_out_of_vocabulary_name_is_not_counted)
    case("substring-trap", a_substring_of_a_stated_name_is_not_counted)

    for name, verdict, detail in results:
        print(f"  {name:<32} {verdict:<16} {detail}", file=sys.stderr)
    bad = [n for n, v, _ in results if v != "passed"]
    print(f"self-test: {len(results) - len(bad)}/{len(results)} passed", file=sys.stderr)
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    ap.error("nothing to do: this module is imported for fabricated_members(); "
             "--self-test is its only command")


if __name__ == "__main__":
    sys.exit(main())
