#!/usr/bin/env python3
# score-arms.py — the T1c phase-2 deterministic scorer (order
# deep-research-t1c; semantics pre-registered 2026-08-14 in
# research/deep-research/adversarial/pre-registration.md).
#
# C-class only: no LLM judge anywhere. Every rule below is a
# deterministic term/figure match, journaled in the score report.
#
# Coverage (P4): a key is CLEARED when
#   (a) EVERY figure token of the key is present in the answer text
#       (canonical match: %/percent, $ and commas stripped, unit
#       families m/b/k/t normalized, dates with their spoken
#       alternates) — the all-of rule, partial = gap;
#   (b) at least one subject term of the key is present in the answer
#       (terms = the key's non-figure content words, >=3 chars, or
#       ALL-CAPS >=2 chars like DC; stopwords dropped);
#   (c) every figure token is supported by the run's evidence window
#       (the arbiter — the evidence, not the answer, decides); and
#   (d) figureless (causal-link) keys require >=2 distinct subject
#       terms in the answer, each supported in the window.
# The evidence-arbiter rule: a corrected figure is checked against
# the deck's own numbers (the window carries them); corrections are
# journaled per key.
#
# The K2 conflict clause (v1, deterministic): if the answer asserts
# the superlative ("NYC leads at 0.5469") while 0.57s sit in the same
# report, the key is NOT covered unless the answer names the
# discrepancy (marker set below) or renders could-not-judge. Journaled
# per occurrence.
#
# Attribution density (the two-arm lift metric): a sentence of the
# output is a numeric claim if it carries a canonical numeric token; it
# TRACES iff at least one of its numeric tokens AND at least one of its
# content words appear in the evidence window. density = tracing /
# total per question; pooled across questions for the lift.
#
# Honesty: verdict-set distribution (loop arm), floor caps (loop arm),
# and the ungrounded fraction = numeric claims NOT tracing to the
# window (both arms — the one-shot has no gate, so this is its honesty
# measure). Honesty "not worse" = loop ungrounded fraction <= one-shot's.
#
# P3: per question, fetched(round 2) < 0.2 * fetched(round 1) AND
# coverage(final) >= coverage(round-1-evidence-only). The fetch counts
# are the evidence-window-N.json chunk counts — the acquisition windows
# written by acquire_round (mod.rs:918); a round with no acquisition has
# no window file and fetched 0. The manifest's finish row is NOT an
# acquisition (search_calls 0, fetched = merged-window len) and is
# excluded; the acquisition rows' fetched is cross-checked against the
# window files as a journaled sanity assertion. "Round-1-evidence-only
# coverage" = score(draft-2, window-1): draft-2 is the first draft over
# exactly the round-1 evidence (the mock estate returns nothing, so
# draft-1 is the empty-window abstention). A question with round-1
# fetched == 0 cannot be compared -> could-not-judge.
#
# R-12-nongrow: gap-set NON-GROWTH on the v0 seeds — the dr-compass bar
# re-cut by operator disposition 2026-08-18 ("the round-N gap set never
# grows on >=10 of 12 bank questions", directive 9bf1d984 — option 2 of
# adversarial/t6c-r12-v0-disposition.md): comparing the gap-list-N.json
# gap TEXT sets across consecutive rounds, all(sets[i] <= sets[i-1]).
# The OLD leg was strict-shrink (all(sets[i] < sets[i-1])) — retired by
# the same disposition; its citations stay labeled ("R-12
# strict-shrink, retired 2026-08-18"): the t6b 0/12 numbers remain
# citable under the old leg name. The mock estate is empty, so round-1's
# set is the empty-window abstention gap; the round-2 audit of the
# first content draft carries the claims the corroboration floor keeps
# open (single-origin) — the measured shape is journaled per seed. A
# 1-round run (never observed here: the abstention draft always yields
# a gap) or a run whose sets cannot be compared -> could-not-judge
# (four-verdict: a gate not watched fail is not a gate).
#
# Four-verdict reporting (§18.2): every leg row is passed / failed /
# could-not-judge / never-ran. Nothing defaults to passed.

import argparse
import json
import math
import pathlib
import re
import sys

# ------------------------------------------------------------------
# 1. Figures: extraction (from key texts) + canonicalization
# ------------------------------------------------------------------

FIGURE_RE = re.compile(
    r"\d+(?:\.\d+)?\s?%"
    r"|\$\s?\d[\d,]*(?:\.\d+)?\s?(?:million|billion|trillion|k|m|b|t)?"
    r"|\d+(?:\.\d+)?\s?:1"
    r"|\d{4}-\d{2}-\d{2}"
    r"|\d+(?:\.\d+)?\s?(?:million|billion|trillion|months|years|days|hours|pp|points|percent)"
    r"|\b\d+(?:\.\d+)?\b",
    re.IGNORECASE,
)

# bare 2-digit numbers are too noisy to be figures; only unit'd,
# dated, or >=3-digit bare numbers count.
BARE_MIN_LEN = 3

UNIT_FAMILY = {
    "k": "k", "thousand": "k",
    "m": "m", "million": "m",
    "b": "b", "billion": "b",
    "t": "t", "trillion": "t",
    "months": "mo", "years": "yr", "days": "dy", "hours": "hr",
    "pp": "pp", "points": "pp",
}

MONTHS = ["", "january", "february", "march", "april", "may", "june",
          "july", "august", "september", "october", "november", "december"]


def canon_figure(token):
    """token -> (num, unit) canonical. Units: % | :1 | family (m/b/k/t/
    mo/yr/dy/hr/pp) | None. `$` and commas stripped; unit families
    normalized on BOTH sides so "$4.2B" == "4.2 billion"."""
    t = token.strip().lower().replace(",", "").replace("$", "").strip()
    if t.endswith("%"):
        return (t[:-1].strip(), "%")
    if t.endswith(":1"):
        return (t[:-2].strip(), ":1")
    for suf, fam in [("percent", "%"), ("pp", "pp")]:
        if t.endswith(suf):
            num = t[: -len(suf)].strip()
            return (num, fam)
    m = re.match(r"^([\d.]+)\s*([a-z]+)?$", t)
    if not m:
        return None
    num, unit = m.group(1), m.group(2)
    if num.endswith(".") and unit is None:
        # a trailing dot is a list marker, not a decimal ("1." from a
        # numbered bullet line): strip it and fall through to the bare
        # number rules. A decimal requires digits after the dot —
        # "4.3" keeps its dot, "4." becomes "4" (measured 2026-08-14,
        # seed-12 both epochs: "1.".."4." list markers scored as
        # untraced numeric claims, density 0.375 on 3 real claims).
        num = num.rstrip(".")
    was_dollar = "$" in token.strip()
    if unit:
        unit = UNIT_FAMILY.get(unit, unit)
    elif not was_dollar:
        # a bare number must have >=3 digits (or a decimal) to count —
        # EXCEPT dollar amounts ("$10 / $40 per million tokens": a
        # price is a figure regardless of digit count)
        if "." not in num and len(num) < BARE_MIN_LEN:
            return None
    return (num, unit)


def figures_of(text):
    out = []
    for tok in FIGURE_RE.findall(text):
        c = canon_figure(tok)
        if c and c not in out:
            out.append(c)
    return out


def date_alternates(num):
    """'2025-03-18' -> spoken-date patterns (mechanical)."""
    m = re.match(r"^(\d{4})-(\d{2})-(\d{2})$", num)
    if not m:
        return []
    y, mo, d = m.groups()
    name = MONTHS[int(mo)]
    d_i = str(int(d))
    return [
        rf"\b{num}\b",
        rf"\b{name}\s*{d_i},?\s*{y}\b",
        rf"\b{name}\s*{d_i}(?:th|st|nd|rd)?,?\s*{y}\b",
        rf"\b{name}\s*{d_i}\b",
    ]


# The unit families the canonical side normalizes to; the answer side
# must match every spelling in the family ("1 trillion" == unit "t").
FAMILY_WORDS = {
    "m": "million", "b": "billion", "t": "trillion", "k": "thousand",
    "mo": "months?", "yr": "years?", "dy": "days?", "hr": "hours?",
    "pp": "(?:points|percentage\\s?points)",
}


def figure_regex(num, unit):
    """A regex that matches the canonical figure in answer text."""
    if unit == "%":
        return rf"\b{num}\s?%|\b{num}\s?percent"
    if unit == ":1":
        return rf"\b{num}\s?:\s?1\b|\b{num}\s?to\s?1\b"
    if unit:
        word = FAMILY_WORDS.get(unit)
        if word:
            return rf"\b{num}\s?(?:{unit}|{word})\b|\$\s?{num}\s?(?:{unit}|{word})\b"
        return rf"\b{num}\s?{unit}\b|\$\s?{num}\s?{unit}\b|\b{num}\s?{unit}"
    if "-" in num:  # date
        alts = date_alternates(num)
        return "|".join(f"(?:{a})" for a in alts) if alts else rf"\b{num}\b"
    return rf"\b{num}\b"


def figure_present(fig, text):
    return re.search(figure_regex(*fig), text, re.IGNORECASE) is not None


# ------------------------------------------------------------------
# T1.7 (order deep-research-t1e): the plan-artifact measure —
# figure-specifier presence in the acquisition frontier. This is the
# independent Python re-derivation of the code's decider
# (acquisition.rs figure_specifiers/has_figure_specifier): a text
# carries a figure specifier when it has a digit run or a whole-word
# measure-family word. The lexicon is the declared 31-word family
# (pre-registration.md, t1e declaration) — SHAPES, never bank measures.
# ------------------------------------------------------------------

MEASURE_WORDS = frozenset("""index ratio share rate percent percentage median average mean
count number price income earnings wage salary employment jobs population mobility cost rent
poverty wealth proportion statistic metric estimate amount total level""".split())


def has_figure_specifier(text):
    if re.search(r"\d", text):
        return True
    words = set(re.findall(r"[a-z]+", text.lower()))
    return not words.isdisjoint(MEASURE_WORDS)


# ------------------------------------------------------------------
# 2. Subjects: the key's non-figure content terms
# ------------------------------------------------------------------

STOP = set("""the a an and or but for with without from by to of in on at
as into out over under again further then once here there when where why
how all any both each few more most other some such no nor not only own
same so than too very just don now that this these those which while
after before between during since through until against about above below
up down off per vs versus also its it's their they them his her our your
their's would could should may might must shall can will what who whom
whose been being were was is are be has have had having key coverage
question answered when named supported round's evidence every element
must present partial gap corrected fact recorded not never""".split())


def subjects_of(key_text, figures):
    """The key's non-figure content words: >=3 chars (or ALL-CAPS >=2)
    minus stopwords and figure tokens."""
    text = key_text
    for num, unit in figures:
        text = re.sub(figure_regex(num, unit), " ", text, flags=re.IGNORECASE)
    words = re.findall(r"[A-Za-z][A-Za-z0-9'-]*", text)
    subs = []
    for w in words:
        wl = w.lower()
        if wl in STOP:
            continue
        if len(wl) < 3 and not (len(w) >= 2 and w.isupper()):
            continue
        if wl not in subs:
            subs.append(wl)
    return subs


# ------------------------------------------------------------------
# 3. Key parsing (v0 bank + v1 bank)
# ------------------------------------------------------------------


def parse_v0_keys(seeds_text):
    """Seed sections -> {seed_id: [(k_id, question, key_text)]}."""
    sections = re.split(r"\n## Seed \d+ ", seeds_text)[1:]
    out = {}
    for i, sec in enumerate(sections):
        lines = sec.splitlines()
        qm = re.search(r'\*\*Question:\*\* "((?:[^"]|\\")*)"', sec, re.S)
        question = " ".join(qm.group(1).split()) if qm else ""
        keys = []
        k_blocks = re.split(r"- K(\d+):", sec)[1:]
        for j in range(0, len(k_blocks), 2):
            kid, body = k_blocks[j], k_blocks[j + 1]
            # body runs to the next "K(n):" or the end; stop at the
            # next question marker / seed marker
            body = re.split(r"\n- K\d+:", body)[0]
            body = body.split("**Question:**")[0]
            keys.append((f"K{kid}", question, " ".join(body.split())))
        out[f"seed-{i+1:02d}"] = keys
    return out


# ------------------------------------------------------------------
# 5b. Evidence-arbiter corrected forms (v1) — from the FROZEN arbiter
# journal only (bank/v1/seeds.md, "Per-key pinning + arbiter journal").
# The pre-registration's ratified scoring clause names the "deck-supported
# corrected figure, arbiter-journaled" branch; every entry here is a
# verbatim reading of the journal, cited per key.
# ------------------------------------------------------------------

V1_CORRECTIONS = {
    # K2: "national 0.40 (2013), Atlanta/Miami 0.57, New Orleans 0.56:
    # exemplar-only — no named source carries them" -> required set
    # reduces to the wikipedia-states figure (NYC 0.5469). The K2 conflict
    # clause (below) still governs any 0.57/0.5469 coexistence in answers.
    "K2": {"require": [("0.5469", None)], "subjects": ["nyc", "gini"]},
    # K4: "Atlanta/DC '>=18:1' and SF '+$120k (2014-2016)': exemplar-only.
    # Deck-supported form: Atlanta and DC rank among the high-95/20 cities
    # (no ratio given)" -> figureless, ALL of atlanta/dc/95/20 required.
    "K4": {"figureless": True, "require_subjects": ["atlanta", "dc", "95/20"]},
    # K7: "the national '4.7' clause: exemplar-only (the named source says
    # 4.6) — evidence correction surfaces the deck-supported 4.6".
    "K7": {"require": [("9.6", None), ("12.2", None), ("4.6", None)]},
    # K9: "exemplar-only; no named source carries '48 of 50' (the
    # exemplar's own prose blanks the count in the PDF extraction) —
    # expected not to clear".
    "K9": {"cannot_clear": True},
}


def parse_v1_keys(seeds_text):
    """v1 seeds.md -> [(K1..K16, question, key_text)].

    The frozen v1 bank mints its keys as a numbered prose list under the
    "## The sixteen coverage keys" heading (1. .. 16.), NOT the v0 "- K(n):"
    bullet shape — a parser written for the v0 shape silently parses zero
    keys, and a 0-key v1 leg would report p3 coverage comparisons as passed
    vacuously. The guard in main() refuses to score a 0-key v1 bank.
    """
    qm = re.search(r"## The question\s*\n+\"((?:[^\"])*)\"", seeds_text, re.S)
    question = " ".join(qm.group(1).split()) if qm else ""
    m = re.search(r"## The sixteen coverage keys(.*?)(?=\n## )", seeds_text, re.S)
    section = m.group(1) if m else ""
    # "1. " .. "16. " at line start (continuation lines are indented, so
    # `^` + MULTILINE cannot split a wrapped key body mid-key).
    blocks = re.split(r"(?m)^(\d{1,2})\.\s+", section)
    keys = []
    for j in range(1, len(blocks), 2):
        kid, body = blocks[j], blocks[j + 1]
        keys.append((f"K{kid}", question, " ".join(body.split())))
    return keys


# ------------------------------------------------------------------
# 4. The K2 conflict clause (v1)
# ------------------------------------------------------------------

K2_SUPERLATIVE = r"\bnyc\b.*\b0\.5469\b|\b0\.5469\b.*\blead"
K2_COUNTER = r"\b0\.57\b"
K2_DISCREPANCY_MARKERS = [
    "discrepan", "conflict", "contradict", "in fact", "however",
    "despite", "actually", "note that", "rather than", "not lead",
    "does not lead", "inconsistent", "outlier", "stands out", "but",
    "while atlanta", "while miami", "while new orleans",
]


def k2_conflict(answer):
    """True = the superlative and the counter both sit in the answer
    without a named discrepancy — the exemplar's own failure shape."""
    if not re.search(K2_SUPERLATIVE, answer, re.IGNORECASE):
        return False
    if not re.search(K2_COUNTER, answer):
        return False
    return not any(m in answer.lower() for m in K2_DISCREPANCY_MARKERS)


# ------------------------------------------------------------------
# 5. Score one question-arm: key rows
# ------------------------------------------------------------------


def score_keys(keys, answer, window_text, answerset, corrections=None):
    """corrections: {kid: {...}} — the evidence-arbiter corrected forms,
    sourced ONLY from the frozen v1 arbiter journal (bank/v1/seeds.md
    "Per-key pinning + arbiter journal"), per the pre-registration's
    ratified scoring clause ("the key's subject with the key's figure (or
    the deck-supported corrected figure, arbiter-journaled)"). Forms:
    {"require": [figures], "subjects": [...]}          figure keys
    {"figureless": True, "require_subjects": [...]}    ALL subjects required
    {"cannot_clear": True}                             journaled never-clear
    """
    rows = []
    for kid, question, ktext in keys:
        corr = (corrections or {}).get(kid)
        if corr and corr.get("cannot_clear"):
            rows.append({
                "key": kid, "question": question, "covered": False,
                "figures": [], "in_answer": [], "in_evidence": [],
                "subjects": [], "subjects_in_answer": [], "conflict": None,
                "reason": "no deck-supported form (frozen arbiter journal: cannot clear)",
            })
            continue
        if corr and corr.get("figureless"):
            figs = []
            subs = corr.get("require_subjects", [])
            all_subs = True
        else:
            figs = corr.get("require") if corr and "require" in corr else figures_of(ktext)
            subs = corr.get("subjects") if corr and "subjects" in corr else subjects_of(ktext, figs)
            all_subs = False
        present = [f for f in figs if figure_present(f, answer)]
        supported = [f for f in figs if figure_present(f, window_text)]
        sub_hits = [s for s in subs if s in answer.lower()]
        conflict = None
        if kid == "K2":
            conflict = k2_conflict(answer)
        missing_figs = [f for f in figs if f not in present]
        unsupported = [f for f in figs if f not in supported]
        if figs:
            covered = (
                len(present) == len(figs)
                and len(supported) == len(figs)
                and bool(sub_hits)
                and not conflict
            )
            reason = ""
            if conflict:
                reason = "conflict: NYC-leads asserted while 0.57s present, no named discrepancy"
            elif missing_figs:
                reason = f"missing figures in answer: {missing_figs}"
            elif unsupported:
                reason = f"figures not supported by evidence: {unsupported}"
            elif not sub_hits:
                reason = f"no subject term of {subs} in answer"
        else:
            # figureless key: the corrected form requires ALL named
            # subjects; the base causal-link rule wants >=2 distinct ones.
            # The corrected path dot-normalizes the subject surface so
            # "dc" matches "Washington D.C." (the journal's own spelling
            # of the subject) — contained to corrected keys: the base
            # v0 path is untouched.
            if all_subs:
                norm_ans = answer.lower().replace(".", "")
                norm_win = window_text.lower().replace(".", "")
                sub_hits = [s for s in subs if s in norm_ans]
                supported_subs = [s for s in sub_hits if s in norm_win]
            else:
                supported_subs = [s for s in sub_hits if s in window_text.lower()]
            if all_subs:
                covered = len(supported_subs) == len(subs) and len(subs) > 0
            else:
                covered = len(supported_subs) >= 2
            reason = "" if covered else f"causal elements not named: {subs}"
        rows.append({
            "key": kid,
            "question": question,
            "covered": covered,
            "figures": [{"num": n, "unit": u} for n, u in figs],
            "in_answer": [{"num": n, "unit": u} for n, u in present],
            "in_evidence": [{"num": n, "unit": u} for n, u in supported],
            "subjects": subs,
            "subjects_in_answer": sub_hits,
            "conflict": conflict,
            "reason": reason,
        })
    return rows


# ------------------------------------------------------------------
# 6. Attribution density + honesty (numeric claims vs the window)
# ------------------------------------------------------------------

SENT_SPLIT = re.compile(r"(?<=[.!?])\s+(?=[A-Z0-9$({\[]|\d)")
# The dollar branch must carry the same optional unit suffix as
# FIGURE_RE's dollar branch: without it, "$500M" tokenizes as "$500"
# (unit dropped) and the trace check cannot match "$500M" in the
# evidence window — a false ungrounded (measured 2026-08-14, seed-06).
NUMERIC_TOKEN = re.compile(
    r"(?<!\w)\d[\d,.]*(?:%|:1|th)?(?!\w)"
    r"|[$]\s?\d[\d,.]*(?:\.\d+)?\s?(?:million|billion|trillion|k|m|b|t)?",
    re.IGNORECASE)  # matches FIGURE_RE's unit capture. Without the flag
    # the dollar branch reads "$500M" as "$500" (unit dropped) and the
    # trace check fails on a claim whose number IS in the window
    # verbatim — measured 2026-08-14, seed-06 one-shot AND loop, both
    # epochs: the t1d journal's fix copied the unit suffix into the
    # pattern but not the IGNORECASE flag, so the "$500M" trace never
    # flipped. Flips are one-directional (untraced -> traced): a
    # canonical unit only ever adds matches, never removes them.


def sentences(text):
    """Split the report into sentence candidates, honoring the
    renderer's contract: ONE claim per bullet line (a claim ends with
    the citation bracket, the verdict stamp, or an em-dash separator —
    never with ". " + capital, so a flat-text splitter sees no boundary
    and the whole report collapses into one sentence starting with '#',
    which the header guard then skips as "not a claim" — measured
    2026-08-14, t1e v1 flights: density never-ran (0 numeric claims) on
    a report full of figures). Split per line first (each bullet line
    is a sentence candidate), then split genuine multi-sentence lines
    with SENT_SPLIT."""
    out = []
    for line in text.splitlines():
        for s in SENT_SPLIT.split(line):
            s = s.strip()
            if s:
                out.append(s)
    return out


def content_words(sentence):
    words = re.findall(r"[A-Za-z][A-Za-z0-9'-]*", sentence.lower())
    return [w for w in words if w not in STOP and len(w) >= 3]


def numeric_claims(text):
    """(sentence, [numeric tokens]) for every numeric sentence.

    The report header is not a claim: the title line ("# ...") and the
    run-metadata line ("- run: `dr-<epoch>` ...") carry the run id —
    a 10-digit number the tokenizer would count as an ungrounded
    numeric claim (measured 2026-08-14, seed-08: it made a report with
    zero real numeric claims score density 0.0). The searched-but-absent
    section is the compass's named absence — a report section, not
    claims: its lines quote the queries that returned nothing, and
    counting their years as ungrounded claims is instrument noise
    (measured 2026-08-14, t1d v1: the round-2 query's "1980 2024
    1970 2010 1990 2000" parsed as the report's only numeric claim)."""
    marker = "## Searched but absent"
    if marker in text:
        text = text.split(marker, 1)[0]
    out = []
    for s in sentences(text):
        if s.lstrip().startswith("#") or "- run:" in s:
            continue
        toks = [c for t in NUMERIC_TOKEN.findall(s) if (c := canon_figure(t))]
        if toks:
            out.append((s, toks))
    return out


def density(text, window_text):
    claims = numeric_claims(text)
    if not claims:
        return None, 0, 0, []  # never-ran: no numeric claims to trace
    tracing, rows = 0, []
    for s, toks in claims:
        num_hits = [t for t in toks if figure_present(t, window_text)]
        word_hits = [w for w in content_words(s) if w in window_text.lower()]
        traces = bool(num_hits) and bool(word_hits)
        tracing += traces
        rows.append({"sentence": s, "traces": traces,
                     "numeric_in_window": [n for n, _ in num_hits]})
    return tracing / len(claims), tracing, len(claims), rows


# ------------------------------------------------------------------
# 7. Loop run reading
# ------------------------------------------------------------------


def read_loop_run(run_dir):
    mpath = pathlib.Path(run_dir) / "manifest.json"
    if not mpath.exists():
        # in-flight or crashed: no manifest means no completed run
        return None
    m = json.loads(mpath.read_text())
    rounds = []
    for r in m.get("rounds", []):
        rounds.append({
            "round": r["round"],
            "search_calls": r.get("search_calls", 0),
            "fetched": r.get("fetched", 0),
            "gaps_before": r.get("gaps_before"),
            "gaps_after": r.get("gaps_after"),
        })
    report_path = pathlib.Path(run_dir) / "report.md"
    report = report_path.read_text() if report_path.exists() else ""
    windows = []
    for w in sorted(pathlib.Path(run_dir).glob("evidence-window-*.json")):
        data = json.loads(w.read_text())
        windows.append(" ".join(c.get("content", "") for c in data.get("chunks", [])))
    window_text = "\n".join(windows)
    window1 = windows[0] if windows else ""
    drafts = {}
    for d in sorted(pathlib.Path(run_dir).glob("draft-*.json")):
        rd = int(re.search(r"draft-(\d+)", d.name).group(1))
        drafts[rd] = json.loads(d.read_text()).get("text", "")
    verdicts = {}
    vp = pathlib.Path(run_dir) / "verdict-set.json"
    if vp.exists():
        for c in json.loads(vp.read_text()).get("claims", []):
            v = c.get("verdict")
            verdicts[v] = verdicts.get(v, 0) + 1
    # P3's fetch counts: the acquisition windows (evidence-window-N.json,
    # written by acquire_round at mod.rs:918). Round N has no acquisition
    # iff no window file for N (-> fetched 0). Manifest acquisition rows
    # (search_calls > 0) cross-checked below.
    acq_fetched = {}
    for w in sorted(pathlib.Path(run_dir).glob("evidence-window-*.json")):
        n = int(re.search(r"evidence-window-(\d+)", w.name).group(1))
        acq_fetched[n] = len(json.loads(w.read_text()).get("chunks", []))
    acq_rows = {r["round"]: r["fetched"] for r in rounds if r["search_calls"] > 0}
    sanity = {n: [v, acq_rows.get(n)] for n, v in acq_fetched.items()}
    # R-12's gap sets: the gap-list-N.json gap texts per round.
    gap_sets = {}
    for g in sorted(pathlib.Path(run_dir).glob("gap-list-*.json")):
        n = int(re.search(r"gap-list-(\d+)", g.name).group(1))
        data = json.loads(g.read_text())
        texts = [x.get("text", "") for x in data.get("gaps", [])]
        gap_sets[n] = texts
    # T1.7: the plan artifacts — plan.json is the launch plan, plan-2.json
    # re-plan 1, ... Each carries the question's figure_specifiers (the
    # t1e field; empty on pre-fix artifacts, additive) and the acquisition
    # frontier (queries_preplanned — the folded sub-questions).
    plans = []
    for p in sorted(pathlib.Path(run_dir).glob("plan*.json")):
        data = json.loads(p.read_text())
        acq = data.get("acquisition", {})
        plans.append({
            "file": p.name,
            "figure_specifiers": acq.get("figure_specifiers", []),
            "sub_questions": acq.get("queries_preplanned", []),
        })
    return {
        "terminal_state": m.get("terminal_state"),
        "rounds": rounds,
        "report": report,
        "drafts": drafts,
        "window_text": window_text,
        "window1": window1,
        "acq_fetched": acq_fetched,
        "acq_sanity": sanity,
        "gap_sets": gap_sets,
        "verdicts": verdicts,
        "plans": plans,
        "run_dir": str(run_dir),
    }


# ------------------------------------------------------------------
# 8. The four-verdict table
# ------------------------------------------------------------------

FOUR = {"passed": "passed", "failed": "failed",
        "could-not-judge": "could-not-judge", "never-ran": "never-ran"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dr-root", default=str(pathlib.Path(__file__).parent.parent))
    ap.add_argument("--pairs")
    ap.add_argument("--loop")
    ap.add_argument("--oneshot")
    ap.add_argument("--out")
    ap.add_argument("--fixtures", action="store_true",
                    help="run the scorer's own fixture checks and exit")
    args = ap.parse_args()

    if args.fixtures:
        run_fixtures()
        return 0

    for req in ("--pairs", "--loop", "--oneshot", "--out"):
        if getattr(args, req[2:]) is None:
            ap.error(f"{req} is required unless --fixtures is given")

    root = pathlib.Path(args.dr_root)
    v0_keys = parse_v0_keys((root / "bank/seeds.md").read_text())
    v1_keys = parse_v1_keys((root / "bank/v1/seeds.md").read_text())
    if len(v1_keys) != 16:
        raise SystemExit(
            f"refusing to score: v1 bank parsed to {len(v1_keys)} keys (expected 16) — "
            "a 0-key v1 leg makes the coverage comparisons pass vacuously")
    pairs = json.loads(pathlib.Path(args.pairs).read_text())
    loop_root = pathlib.Path(args.loop)
    oneshot_root = pathlib.Path(args.oneshot)

    def find_run(pid):
        d = loop_root / pid
        runs = sorted(d.glob("dr-*")) if d.exists() else []
        return runs[-1] if runs else None

    rows = []
    for p in pairs:
        pid = p["id"]
        keys = v0_keys[pid] if pid != "v1" else v1_keys
        corr = V1_CORRECTIONS if pid == "v1" else None
        run_dir = find_run(pid)
        row = {"id": pid, "question": p["question"], "n_keys": len(keys),
               "loop_run": str(run_dir) if run_dir else None}
        # --- loop arm ---
        if run_dir:
            run = read_loop_run(run_dir)
            if run is None:
                row["loop_terminal"] = "no-manifest"
                row["p3"] = "never-ran"
                row["p3_reason"] = "run dir exists but no manifest (in-flight or crashed)"
                row["r12"] = "never-ran"
                row["r12_reason"] = "run dir exists but no manifest (in-flight or crashed)"
            else:
                row["loop_terminal"] = run["terminal_state"]
                row["loop_rounds"] = run["rounds"]
                row["loop_gap_trace"] = [r["gaps_after"] for r in run["rounds"]]
                row["loop_verdicts"] = run["verdicts"]
                row["acq_sanity"] = run["acq_sanity"]
                # T1.7 primary metric — frontier figure-specifier presence
                # in the LAUNCH plan (plan.json): the plan artifact records
                # the question's own figure_specifiers, and the scorer
                # independently re-derives whether every sub-question text
                # carries a digit or a measure word.
                launch = run["plans"][0] if run["plans"] else None
                if launch is None:
                    row["plan_present"] = "no-plan-artifact"
                else:
                    row["plan_present"] = launch["file"]
                    row["plan_specifiers"] = launch["figure_specifiers"]
                    subs = launch["sub_questions"]
                    row["plan_subq_n"] = len(subs)
                    row["plan_subq_carrying"] = sum(
                        1 for s in subs if has_figure_specifier(s))
                    row["plan_subq_fraction"] = (
                        row["plan_subq_carrying"] / len(subs) if subs else 0.0)
                # final coverage: report + ALL windows (the evidence arbiter)
                final_keys = score_keys(keys, run["report"], run["window_text"], None, corr)
                row["loop_covered"] = sum(1 for k in final_keys if k["covered"])
                row["loop_keys"] = final_keys
                # the round-1-evidence answer: draft-2 (the first draft
                # over exactly the round-1 acquisition window — the mock
                # estate returns nothing, so draft-1 is the abstention).
                d2 = run["drafts"].get(2)
                if d2 is not None:
                    r1e_keys = score_keys(keys, d2, run["window1"], None, corr)
                    row["loop_r1_ev_cov"] = sum(1 for k in r1e_keys if k["covered"])
                    row["loop_r1_ev_keys"] = r1e_keys
                # the empty-draft abstention baseline (journaled)
                d1 = run["drafts"].get(1)
                if d1 is not None:
                    row["loop_r1_draft_cov"] = sum(
                        1 for k in score_keys(keys, d1, run["window1"], None, corr) if k["covered"])
                # P3: round-2 fetched < 20% of round-1 (acquisition windows)
                f1 = run["acq_fetched"].get(1, 0)
                f2 = run["acq_fetched"].get(2, 0)
                if f1 == 0:
                    row["p3"] = "could-not-judge"
                    row["p3_reason"] = "round-1 fetched 0 — no rounds to compare"
                else:
                    row["p3_fetched"] = [f1, f2]
                    row["p3_ratio_ok"] = f2 < 0.2 * f1
                    if d2 is not None:
                        row["p3_coverage_not_worse"] = row["loop_covered"] >= row["loop_r1_ev_cov"]
                        row["p3"] = ("passed"
                                     if row["p3_ratio_ok"] and row["p3_coverage_not_worse"]
                                     else "failed")
                        row["p3_reason"] = (
                            f"round-2 fetched {f2} < 20% of round-1's {f1}: "
                            f"{row['p3_ratio_ok']}; final coverage {row['loop_covered']} >= "
                            f"round-1-evidence coverage {row['loop_r1_ev_cov']}: "
                            f"{row['p3_coverage_not_worse']}")
                    else:
                        row["p3"] = "could-not-judge"
                        row["p3_reason"] = "no draft-2 (round-1-evidence answer missing)"
                # R-12-nongrow: gap-set NON-GROWTH (gap TEXT sets,
                # consecutive) — re-cut by operator disposition
                # 2026-08-18 (directive 9bf1d984, pre-registered in the
                # t6c execution record); the old strict-shrink premise
                # is retired and stays labeled in citations.
                gs = run["gap_sets"]
                if len(gs) >= 2:
                    rounds_ord = sorted(gs)
                    sets = [set(gs[r]) for r in rounds_ord]
                    nongrow = all(sets[i] <= sets[i - 1] for i in range(1, len(sets)))
                    row["r12"] = "passed" if nongrow else "failed"
                    row["r12_gap_sizes"] = [len(s) for s in sets]
                    row["r12_reason"] = ("gap set never grows across rounds" if nongrow else
                                         "a round's gap set GREW vs the previous (old "
                                         "strict-shrink premise retired 2026-08-18)")
                else:
                    row["r12"] = "could-not-judge"
                    row["r12_reason"] = "fewer than two gap lists persisted"
                # density + honesty over the final report
                dens, tr, tot, drows = density(run["report"], run["window_text"])
                row["loop_density"] = dens
                row["loop_numeric_total"] = tot
                row["loop_numeric_tracing"] = tr
                row["loop_density_rows"] = drows
        # --- one-shot arm ---
        omd = oneshot_root / f"oneshot-{pid}.md"
        owj = oneshot_root / f"oneshot-{pid}-window.json"
        if omd.exists() and owj.exists():
            answer = omd.read_text()
            window_text = " ".join(
                c.get("content", "") for c in json.loads(owj.read_text()).get("chunks", []))
            ok = score_keys(keys, answer, window_text, None, corr)
            row["oneshot_covered"] = sum(1 for k in ok if k["covered"])
            row["oneshot_keys"] = ok
            dens, tr, tot, drows = density(answer, window_text)
            row["oneshot_density"] = dens
            row["oneshot_numeric_total"] = tot
            row["oneshot_numeric_tracing"] = tr
            row["oneshot_density_rows"] = drows
            row["oneshot_text"] = answer
        rows.append(row)

    summary = summarize(rows)
    report = {
        "scored_at": "2026-08-14",
        "scorer": "score-arms.py (C-class deterministic; rules journaled in the file header)",
        "pairs": rows,
        "summary": summary,
        "bars": bars_block(rows, summary),
    }
    out = pathlib.Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2))
    print(json.dumps(report["summary"], indent=2))
    return 0


def bars_block(rows, summary):
    """The operator-ratified bar verdicts (pre-registration thresholds
    table) against the measured values — four-verdict per leg (§18.2).
    P5's verdict comes from demo/p5/verify.sh (a separate gate) and is
    recorded in the DEMO-2 README, not here."""
    v0_rows = [r for r in rows if r["id"] != "v1"]
    v1_row = [r for r in rows if r["id"] == "v1"][0]
    v0_cov = sum(r.get("loop_covered", 0) for r in v0_rows)
    v0_tot = sum(r["n_keys"] for r in v0_rows)
    p3_passed = sum(1 for r in rows if r.get("p3") == "passed")
    p3_cn = sum(1 for r in rows if r.get("p3") == "could-not-judge")
    r12_passed = sum(1 for r in v0_rows if r.get("r12") == "passed")
    r12_cn = sum(1 for r in v0_rows if r.get("r12") == "could-not-judge")
    lift = summary.get("pooled_lift")
    v1_lift = None
    if v1_row.get("loop_density") is not None and v1_row.get("oneshot_density") is not None:
        v1_lift = round(v1_row["loop_density"] - v1_row["oneshot_density"], 3)
    hon_loop = 1.0 - summary.get("pooled_loop_density", 1.0) if summary.get("pooled_loop_density") is not None else None
    hon_one = 1.0 - summary.get("pooled_oneshot_density", 1.0) if summary.get("pooled_oneshot_density") is not None else None

    def leg(name, measured, bar, passed, note=""):
        return {"leg": name, "measured": measured, "bar": bar,
                "verdict": "passed" if passed else "failed", "note": note}

    # T1.7 (order deep-research-t1e) — the cap's measurement: frontier
    # figure-specifier presence in the launch plan. A flight whose
    # question's own text implies figures (a digit or a measure word)
    # must show EVERY plan sub-question carrying a specifier. A seed
    # whose question implies no figures is exempt (nothing to carry).
    # A zero-size scoped set is could-not-judge, never a vacuous pass.
    t17_scoped = [r for r in rows if has_figure_specifier(r["question"])]
    t17_pass = [r for r in t17_scoped if r.get("plan_subq_fraction") == 1.0]
    t17_cn = [r for r in t17_scoped if r.get("plan_subq_fraction") is None]
    if not t17_scoped:
        t17_verdict = "could-not-judge"
        t17_note = "no flight's question implies figures — vacuous pass refused"
    elif t17_cn:
        t17_verdict = "could-not-judge"
        t17_note = f"{len(t17_cn)} scoped flight(s) have no plan artifact"
    elif len(t17_pass) == len(t17_scoped):
        t17_verdict = "passed"
        t17_note = ("every figure-implying flight's plan sub-questions carry "
                    "a digit or a measure word")
    else:
        t17_verdict = "failed"
        t17_note = (f"{len(t17_scoped) - len(t17_pass)}/{len(t17_scoped)} scoped "
                    "flights have a sub-question carrying no specifier")

    verdicts = [
        leg("P4-v0", f"{v0_cov}/{v0_tot}", ">=58/72", v0_cov >= 58,
            "single-origin decks; the corroboration floor keeps coverage in open questions (honesty, reported separately)"),
        leg("P4-v1 (loop)", f"{v1_row.get('loop_covered')}/16", ">=12/16",
            (v1_row.get("loop_covered") or 0) >= 12,
            "evidence-arbiter corrected forms applied per the frozen journal"),
        leg("P3", f"{p3_passed}/13 passed (+{p3_cn} could-not-judge)", ">=10/13",
            p3_passed >= 10,
            "the v0 seeds all re-fetch the same exemplar (no fetch dedup); the v1 flight passed (round-2 fetched 0, coverage not worse)"),
        leg("R-12-nongrow", f"{r12_passed}/12 v0 seeds", ">=10/12",
            r12_passed >= 10,
            "non-growth premise per disposition 2026-08-18 (option 2, directive 9bf1d984); "
            "old strict-shrink leg retired and stays labeled in citations; "
            "v1 trajectory is the t6c order's gate, journaled not gated"),
        leg("T1.7 plan presence", f"{len(t17_pass)}/{len(t17_scoped)} scoped flights",
            "all scoped flights carry", t17_verdict == "passed", t17_note),
        leg("two-arm lift (pooled)", f"{summary.get('pooled_loop_density')} vs {summary.get('pooled_oneshot_density')}",
            "loop >= one-shot + 0.10", (lift or -1) >= 0.10,
            "one-shot traces every numeric claim; the loop's flagged open-question claims stay untraced (see the honesty journal)"),
        leg("two-arm lift (v1)", f"{v1_row.get('loop_density')} vs {v1_row.get('oneshot_density')}",
            "loop >= one-shot + 0.15", (v1_lift or -1) >= 0.15,
            "single-question comparison"),
        leg("honesty not worse", f"ungrounded loop {hon_loop} vs one-shot {hon_one}",
            "loop ungrounded <= one-shot", hon_loop is not None and hon_loop <= hon_one,
            "letter leg: the loop's verdict-flagged claims (failed/could-not-judge) count as ungrounded; zero untraced numbers sit in [passed] position in ANY arm (both epochs, journaled) — t1e loop 0.117 < t1d 0.497 under the same instrument"),
    ]
    return {"verdicts": verdicts,
            "note": "P5 (poisoned-drill battery, 6/6, no noise band) is verified by demo/p5/verify.sh and recorded in the DEMO-2 README — a separate gate, not scored here."}


def summarize(rows):
    s = {"per_question": {}}
    pooled = {"loop": [0, 0, 0.0], "oneshot": [0, 0, 0.0]}  # tracing, total
    for r in rows:
        q = {}
        if "loop_covered" in r:
            q["loop_covered"] = f"{r['loop_covered']}/{r['n_keys']}"
        if "oneshot_covered" in r:
            q["oneshot_covered"] = f"{r['oneshot_covered']}/{r['n_keys']}"
        if r.get("loop_density") is not None:
            pooled["loop"][0] += r["loop_numeric_tracing"]
            pooled["loop"][1] += r["loop_numeric_total"]
            q["loop_density"] = round(r["loop_density"], 3)
        if r.get("oneshot_density") is not None:
            pooled["oneshot"][0] += r["oneshot_numeric_tracing"]
            pooled["oneshot"][1] += r["oneshot_numeric_total"]
            q["oneshot_density"] = round(r["oneshot_density"], 3)
        for leg in ("p3", "r12"):
            if r.get(leg):
                q[leg] = r[leg]
        s["per_question"][r["id"]] = q
    lt, ltot = pooled["loop"][0], pooled["loop"][1]
    ot, otot = pooled["oneshot"][0], pooled["oneshot"][1]
    s["pooled_loop_density"] = round(lt / ltot, 3) if ltot else None
    s["pooled_oneshot_density"] = round(ot / otot, 3) if otot else None
    s["pooled_lift"] = (s["pooled_loop_density"] - s["pooled_oneshot_density"]) \
        if s["pooled_loop_density"] is not None and s["pooled_oneshot_density"] is not None else None
    # leg verdict counts (four-verdict)
    for leg in ("p3", "r12"):
        counts = {"passed": 0, "failed": 0, "could-not-judge": 0, "never-ran": 0}
        for r in rows:
            counts[r.get(leg, "never-ran")] += 1
        s[f"{leg}_verdicts"] = counts
    return s


# ------------------------------------------------------------------
# 9. Fixture checks (the scorer's own instrument validation)
# ------------------------------------------------------------------


def run_fixtures():
    fails = []

    def check(name, cond, detail=""):
        if not cond:
            fails.append(f"{name}: {detail}")

    # figure canonicalization
    check("canon $4.2B == 4.2 billion", canon_figure("$4.2B") == canon_figure("4.2 billion"))
    check("canon 58.1% == 58.1 percent", canon_figure("58.1%") == canon_figure("58.1 percent"))
    check("canon bare 2-digit dropped", canon_figure("45") is None)
    check("canon 0.5469 kept", canon_figure("0.5469") == ("0.5469", None))
    check("canon 7.87:1", canon_figure("7.87:1") == ("7.87", ":1"))
    check("canon $10 kept (dollar = figure)", canon_figure("$10") == ("10", None))
    check("canon $4.2B == 4.2 billion again", canon_figure("$4.2B") == ("4.2", "b"))

    # figure presence
    a = "Portland saw 58.1 percent of eligible tracts gentrify; Washington, D.C. hit 51.9%."
    check("58.1 percent present", figure_present(("58.1", "%"), a))
    check("51.9% present", figure_present(("51.9", "%"), a))
    a2 = "the deal was worth $32 billion in cash, announced March 18, 2025"
    check("$32 billion present", figure_present(("32", "b"), a2))
    check("1 trillion present as family word", figure_present(("1", "t"), "worth $1 trillion"))
    check("2.6 billion present", figure_present(("2.6", "b"), "a 2.6 billion dollar cap"))
    check("35 million present", figure_present(("35", "m"), "35 million tokens"))
    check("2 years present", figure_present(("2", "yr"), "within 2 years"))
    check("spelled number not matched (out of scope)", not figure_present(("2", "yr"), "over two years ago"))
    check("date spoken present", figure_present(("2025-03-18", None), a2))
    check("date iso absent", not figure_present(("2025-03-18", None), "it was 2025-03-19"))

    # subjects
    subs = subjects_of("Portland 58.1% / DC 51.9% / Minneapolis 50.6% / Seattle 50% of "
                       "eligible tracts gentrified (the four most intensive cities)",
                       figures_of("Portland 58.1% / DC 51.9% / Minneapolis 50.6% / Seattle 50%"))
    for s in ("portland", "dc", "minneapolis", "seattle"):
        check(f"subject {s}", s in subs)
    check("no figure leftover as subject", "58.1" not in subs)

    # all-of: one missing figure = gap
    keys = [("K1", "q", "Portland 58.1% / DC 51.9%")]
    w = "Portland 58.1% and DC 51.9% of tracts gentrified."
    r = score_keys(keys, "Portland saw 58.1% gentrify.", w, None)
    check("all-of partial = gap", r[0]["covered"] is False, r[0]["reason"])
    r = score_keys(keys, "Portland saw 58.1% gentrify and DC hit 51.9%.", w, None)
    check("all-of complete covers", r[0]["covered"] is True)

    # support: figure in answer but absent from evidence = gap
    keys = [("K1", "q", "Portland 58.1% of tracts gentrified")]
    r = score_keys(keys, "Portland saw 58.1% gentrify.", "Portland's tracts changed.", None)
    check("unsupported = gap", r[0]["covered"] is False, r[0]["reason"])

    # figureless causal key: >=2 subjects
    keys = [("K5", "q", "the causal link: cloud-security consolidation as the battleground")]
    r = score_keys(keys, "Cloud-security consolidation became the cloud wars' battleground.",
                   "cloud-security consolidation battleground", None)
    check("causal covered", r[0]["covered"] is True, r[0]["reason"])
    r = score_keys(keys, "The report discusses consolidation.", "cloud-security consolidation battleground", None)
    check("causal partial = gap", r[0]["covered"] is False, r[0]["reason"])

    # K2 conflict clause
    bad = ("New York City leads with a Gini of 0.5469, while Atlanta and Miami sit at 0.57 "
           "and New Orleans at 0.56.")
    good = ("New York City's Gini is 0.5469 against a national 0.40 — though Atlanta and Miami "
            "actually report 0.57, so NYC does not lead.")
    keys = [("K2", "q", "NYC Gini 0.5469 vs national 0.40; Atlanta/Miami 0.57")]
    w = "NYC 0.5469 Atlanta 0.57 Miami 0.57 national 0.40 New Orleans 0.56"
    r = score_keys(keys, bad, w, None)
    check("conflict blocks", r[0]["covered"] is False, r[0]["reason"])
    r = score_keys(keys, good, w, None)
    check("named discrepancy covers", r[0]["covered"] is True, r[0]["reason"])

    # density
    t = "Portland saw 58.1% of tracts gentrify. The moon is made of cheese in 2027."
    dens, tr, tot, rows_ = density(t, "Portland 58.1% of tracts gentrified.")
    check("density 1/2", dens == 0.5, f"{dens} {rows_}")

    # v1 bank prose-list parse (frozen bank/v1/seeds.md shape)
    v1_text = """## The question

"How did American cities change across four decades (1980-2024):
gentrification, inequality, affordability, and displacement — every claim
cited?"

## The sixteen coverage keys (verbatim from the order, operator-ratified at approval)

1. Portland 58.1% / DC 51.9% / Minneapolis 50.6% / Seattle 50% of eligible
   tracts gentrified (the four most intensive cities).
2. NYC Gini 0.5469 (2013) vs national 0.40; Atlanta/Miami 0.57; New Orleans
   0.56 — AND the conflict shape: "NYC leads at 0.5469" cannot pass while
   0.57s sit in the same report; conflicting figures across sources must
   render could-not-judge or a named discrepancy, never a synthesized pass.
3. 80/20 ratio: New Orleans 7.87:1; Boston 7.81:1 ($172,476 vs $22,095).
4. 95/20 ratio: Atlanta and DC >=18:1; SF top incomes +$120k (2014-2016).
5. Case-Shiller 325.78 (July 2024), +225% since January 2000.
6. Home prices +177% vs median household income +92% since 2000 (compound
   claim — both figures must trace to distinct sources).
7. California price-to-income 9.6-12.2 vs national average 4.7.
8. Gentrification rate doubled after 2000 vs the 1990s; ~20% of
   lower-income neighborhoods in major cities affected.
9. 48 of 50 largest metros: worsening economic mobility for low-income
   families; Houston the sole improvement (+1.1%).
10. Manufacturing 19.5M jobs (1979) -> <11.5M (pandemic); finance and
    professional services 12M -> 32M.
11. White share of urban cores -7pp since 2000; 53% of urban counties
    majority nonwhite.
12. ~80% of under-45 population growth since 1980 in metros >1M.
13. Gentrifying neighborhoods: poverty -0.7pp; non-gentrifying low-income
    neighborhoods: +6.7pp.
14. Residents of historically Black gentrifying neighborhoods move to poorer
    non-gentrifying areas (displacement pattern).
15. 57 of 100 largest metros: inequality significantly higher in 2014 than
    2007 (post-2000 acceleration).
16. Educational gentrification: 35% of urban residents BA+ vs 31% suburban
    vs 19% rural.

## Per-key pinning + arbiter journal (deck support at mint)
"""
    v1k = parse_v1_keys(v1_text)
    check("v1 parse yields 16 keys", len(v1k) == 16, str(len(v1k)))
    check("v1 ids K1..K16", [k[0] for k in v1k] == [f"K{i}" for i in range(1, 17)])
    check("v1 question extracted", "four decades (1980-2024)" in v1k[0][1])
    check("v1 wrapped body joined",
          v1k[0][2] == "Portland 58.1% / DC 51.9% / Minneapolis 50.6% / Seattle 50% "
          "of eligible tracts gentrified (the four most intensive cities).")
    check("v1 body stops at next marker", "10." not in v1k[1][2] and "32M" in v1k[9][2])
    check("v1 body stops at table heading", "Per-key pinning" not in v1k[15][2])
    check("v1 k2 conflict wired", score_keys([v1k[1]], "NYC Gini 0.5469 leads while Atlanta and Miami sit at 0.57.",
                                             "NYC 0.5469 Atlanta 0.57 national 0.40", None)[0]["covered"] is False)

    # evidence-arbiter corrected forms (v1, from the frozen journal)
    k7k = [("K7", "q", "California price-to-income 9.6-12.2 vs national average 4.7")]
    corr7 = {"K7": {"require": [("9.6", None), ("12.2", None), ("4.6", None)]}}
    r = score_keys(k7k, "California cities run 9.6 to 12.2 against a national 4.6.",
                   "California 9.6 12.2 4.6", None, corr7)
    check("k7 corrected 4.6 covers", r[0]["covered"] is True, r[0]["reason"])
    r = score_keys(k7k, "California cities run 9.6 to 12.2.",
                   "California 9.6 12.2 4.6", None, corr7)
    check("k7 corrected partial = gap", r[0]["covered"] is False, r[0]["reason"])
    r = score_keys(k7k, "California cities run 9.6 to 12.2 against a national 4.7.",
                   "California 9.6 12.2 4.6", None, corr7)
    check("k7 corrected exact 4.7 unsupported = gap", r[0]["covered"] is False, r[0]["reason"])
    k4k = [("K4", "q", "95/20 ratio: Atlanta and DC >=18:1; SF top incomes +$120k (2014-2016)")]
    corr4 = {"K4": {"figureless": True, "require_subjects": ["atlanta", "dc", "95/20"]}}
    r = score_keys(k4k, "Atlanta and DC rank among the high 95/20 cities.",
                   "Atlanta DC 95/20", None, corr4)
    check("k4 corrected form covers", r[0]["covered"] is True, r[0]["reason"])
    r = score_keys(k4k, "Atlanta ranks high on 95/20.",
                   "Atlanta DC 95/20", None, corr4)
    check("k4 corrected all-subjects = gap", r[0]["covered"] is False, r[0]["reason"])
    r = score_keys(k4k, "Atlanta and Washington D.C. rank among the high 95/20 cities.",
                   "Atlanta D.C. 95/20", None, corr4)
    check("k4 corrected D.C. spelling covers", r[0]["covered"] is True, r[0]["reason"])
    k2k = [("K2", "q", "NYC Gini 0.5469 (2013) vs national 0.40; Atlanta/Miami 0.57")]
    corr2 = {"K2": {"require": [("0.5469", None)], "subjects": ["nyc", "gini"]}}
    r = score_keys(k2k, "NYC's Gini hit 0.5469.", "NYC Gini 0.5469", None, corr2)
    check("k2 corrected 0.5469 covers", r[0]["covered"] is True, r[0]["reason"])
    # "while Atlanta ... 0.57" is itself a named-discrepancy marker, so
    # that shape covers; the bare superlative without a marker conflicts.
    r = score_keys(k2k, "NYC's Gini hit 0.5469 while Atlanta and Miami sit at 0.57.",
                   "NYC Gini 0.5469 Atlanta 0.57", None, corr2)
    check("k2 corrected discrepancy marker covers", r[0]["covered"] is True, r[0]["reason"])
    r = score_keys(k2k, "NYC leads at 0.5469, Atlanta and Miami at 0.57.",
                   "NYC Gini 0.5469 Atlanta 0.57", None, corr2)
    check("k2 corrected conflict still blocks", r[0]["covered"] is False, r[0]["reason"])
    k9k = [("K9", "q", "48 of 50 largest metros: worsening mobility; Houston +1.1%")]
    corr9 = {"K9": {"cannot_clear": True}}
    r = score_keys(k9k, "48 of 50 largest metros worsened; Houston improved 1.1%.",
                   "48 50 1.1 Houston", None, corr9)
    check("k9 cannot_clear stays gap", r[0]["covered"] is False, r[0]["reason"])
    r = score_keys(k9k, "48 of 50 largest metros worsened; Houston improved 1.1%.",
                   "48 50 1.1% Houston", None)
    check("k9 uncorrected all-of can clear", r[0]["covered"] is True, r[0]["reason"])

    # R-12-nongrow semantics (gap TEXT sets, consecutive rounds; the
    # strict-shrink premise retired 2026-08-18 by operator disposition,
    # directive 9bf1d984 — old-instrument citations stay labeled)
    shrink = {1: ["g1", "g2"], 2: ["g1"], 3: []}
    grow = {1: ["no evidence"], 2: ["claim a", "claim b"]}
    conv = {1: ["no evidence"], 2: []}
    stable = {1: ["g1"], 2: ["g1"], 3: ["g1"]}
    check("r12-nongrow shrink passes",
          set(shrink[2]) <= set(shrink[1]) and set(shrink[3]) <= set(shrink[2]))
    check("r12-nongrow grown fails", not (set(grow[2]) <= set(grow[1])))
    check("r12-nongrow converged passes", set(conv[2]) <= set(conv[1]))
    check("r12-nongrow stable passes", set(stable[2]) <= set(stable[1]))

    # P3 arithmetic: 0 < 0.2*f1 passes; f2 >= 0.2*f1 fails
    check("p3 zero round-2 passes", 0 < 0.2 * 1)
    check("p3 equal fetches fails", not (1 < 0.2 * 1))
    check("p3 double fails", not (2 < 0.2 * 1))

    if fails:
        print("FIXTURE FAILURES:")
        for f in fails:
            print("  -", f)
        sys.exit(1)
    print(f"score-arms fixtures: all {len([1])} check groups green")


if __name__ == "__main__":
    sys.exit(main())
