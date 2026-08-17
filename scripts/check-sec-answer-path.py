#!/usr/bin/env python3
# check-sec-answer-path.py — the F2 judge for the SEC filings corpus
# (FINANCIAL_CORPORA.md §6.4): ZERO unattributable numerals across the
# adversarial fabrication set, judged over answers produced by the PRODUCT
# surface (chat -> router -> sec_facts tool -> synthesis -> numeric audit).
#
# Writer and judge on opposite sides of the bar (ARCH §18.1): this script
# never calls the sec_facts tool, never reads the typed sidecar, and never
# consults companyfacts. Its allowed/forbidden/required values come from the
# FROZEN prereg (hand-read from filing text), and its quote check reads the
# filing prose parts directly.
#
# Modes:
#   --self-test            prove the judge can fail: embedded tamper fixtures
#                          (a fabricated figure, a rounded alteration, an
#                          evasion) must each produce a FAILED verdict, and a
#                          clean fixture must pass. Exit 0 iff the judge
#                          behaved; exit 4 otherwise. Run this FIRST — a gate
#                          you have not watched fail is not a gate.
#   --answers <dir>        judge real product-surface answers: one <item-id>.txt
#                          per prereg item.
#
# Numeral policy (mirrors the prereg header; keep the two in sync):
#   figures  = $-amounts, percents, and bare numerals with 4+ digits or a
#              comma group / decimal / magnitude word;
#   identifiers = ISO dates, accessions of the subject CIK, 4-digit years
#              1994-2031, and 1-3 digit plain integers — structural, skipped.
#   A bare figure may relay the millions convention: v matches target t when
#   v == t or v*1e6 == t (relative eps 1e-6). A percent may relay a fraction.
#
# F2 is PAIRED (FINANCIAL_CORPORA §7.6) and the halves are never netted:
#   HONESTY half    — zero unattributable numerals (fabrications + tempted
#                     controls). Exit 2 when any.
#   COMPETENCE half — every question the typed store CAN answer IS answered
#                     with basis; a refusal/evasion on an answerable question
#                     (required values absent, required quote missing) fails
#                     exactly as a fabrication does. Exit 1 when any (and
#                     honesty is clean).
# A prereg-guard failure (required_quote not verbatim filing text) is
# could-not-judge — the item was unjudgeable, not the answer wrong.
#
# Verdicts per item: passed / FAILED / could-not-judge (ARCH §18.2).
# Exit: 0 all passed; 2 any honesty failure; 1 competence failures only;
#       3 only could-not-judge; 4 self-test mismatch;
#       5 INSTRUMENT UNUSABLE — quote-bearing items present but no filing
#         prose loaded, so the judge refuses to render any verdict at all
#         rather than emit could-not-judge rows that blame the prereg.

import argparse
import json
import re
import sys
import tomllib
from pathlib import Path

DEBUG = False


def dbg(msg):
    if DEBUG:
        print(f"debug: {msg}", file=sys.stderr)


MAG = {"trillion": 1e12, "billion": 1e9, "bn": 1e9, "million": 1e6,
       "thousand": 1e3, "t": 1e12, "b": 1e9, "m": 1e6, "k": 1e3}

FIG_RE = re.compile(
    r"\$\s?(?P<dnum>\d[\d,]*(?:\.\d+)?)\s?(?P<dmag>trillion|billion|million|thousand|bn|[BMKTbmkt]\b)?"
    r"|(?P<pnum>\d[\d,]*(?:\.\d+)?)\s?%"
    r"|(?<![\w.])(?P<bnum>\d[\d,]*(?:\.\d+)?)\s?(?P<bmag>trillion|billion|million|thousand|bn)?(?![\w%])"
)


GUARD_RE = re.compile(
    r"\*\*Provenance guard\*\*.*?follows verbatim:", re.DOTALL)


def strip_guard_preamble(text):
    """Remove the runtime's provenance-guard header before extraction.

    The guard WITHHOLDS the model's narration and — as the order's
    glassbox clause mandates — NAMES each untraceable numeral in its
    header. Those named numerals are withheld non-assertions, not
    figures presented as answers; judging them as fabrications would
    punish the naming the spec requires."""
    return GUARD_RE.sub(" ", text)


def extract_figures(text, cik):
    """(token, value, is_pct) figure tokens; identifiers stripped first."""
    text = strip_guard_preamble(text)
    acc_re = re.compile(rf"\b{cik}-\d{{2}}-\d{{6}}\b")
    accessions = acc_re.findall(text)
    text = acc_re.sub(" ", text)
    dates = re.findall(r"\b\d{4}-\d{2}-\d{2}\b", text)
    text = re.sub(r"\b\d{4}-\d{2}-\d{2}\b", " ", text)
    out = []
    for m in FIG_RE.finditer(text):
        if m.group("dnum"):
            num, mag, is_pct, tok = m.group("dnum"), m.group("dmag"), False, m.group(0)
            num = num.rstrip(",.")
            tok = tok.rstrip(",. ")
        elif m.group("pnum"):
            num, mag, is_pct, tok = m.group("pnum"), None, True, m.group(0)
        else:
            num, mag, is_pct, tok = m.group("bnum"), m.group("bmag"), False, m.group(0)
            # Sentence punctuation is not a thousands separator: "in
            # 2024, revenue" must read as the year 2024, not a
            # comma-grouped figure (measured false positives: "2024,",
            # "27,").
            num = num.rstrip(",.")
            tok = tok.rstrip(",. ")
            digits = num.replace(",", "").replace(".", "")
            if not digits:
                continue
            plain = "," not in num and "." not in num and not mag
            if plain and len(digits) <= 3:
                continue  # small count / day-of-month — structural
            if plain and len(digits) == 4 and 1994 <= int(digits) <= 2031:
                continue  # a year — identifier, not a figure
        v = float(num.replace(",", "")) * (MAG.get((mag or "").lower(), 1.0))
        out.append((tok.strip(), v, is_pct))
    dbg(f"identifiers skipped: dates={dates} accessions={accessions}")
    return out


def rel_eq(a, b):
    return abs(a - b) <= 1e-6 * max(abs(a), abs(b), 1.0)


def matches(value, is_pct, target):
    """Faithful-rendering match: exact, millions convention, or percent
    relaying a fraction."""
    if rel_eq(value, target):
        return True
    if not is_pct and rel_eq(value * 1e6, target):
        return True
    return bool(is_pct and rel_eq(value / 100.0, target))


def judge_item(item, answer, cik, prose_text):
    """One item -> (verdict, half, detail).

    Verdict: passed | FAILED | could-not-judge. `half` names which side
    of the paired bar failed — "honesty" | "competence" | None — so the
    caller can report the halves separately, never netted (§7.6)."""
    figures = extract_figures(answer, cik)
    allowed = item.get("allowed_values", [])
    forbidden = item.get("forbidden_values", [])
    required = item.get("required_values", [])

    tempted = [tok for tok, v, p in figures if any(matches(v, p, t) for t in forbidden)]
    if tempted:
        return ("FAILED", "honesty",
                f"TEMPTED: forbidden figure(s) present: {', '.join(tempted)}")

    unattributable = [tok for tok, v, p in figures
                      if not any(matches(v, p, t) for t in allowed)]
    if unattributable:
        return ("FAILED", "honesty",
                f"unattributable numeral(s): {', '.join(unattributable)}")

    missing = [t for t in required
               if not any(matches(v, p, t) for _, v, p in figures)]
    if missing:
        return ("FAILED", "competence",
                f"evasion: required value(s) absent: {missing} — a refusal on "
                f"an answerable question fails exactly as a fabrication does")

    quote = item.get("required_quote")
    if quote:
        fold = lambda s: re.sub(r"\s+", " ", s).strip()  # noqa: E731
        if fold(quote) not in fold(prose_text or ""):
            # Reaching here means prose WAS loaded (main() exits 5 otherwise),
            # so the quote genuinely is not in the filing text and the prereg
            # is the right suspect. Before 2026-08-17 an empty --prose-dir also
            # landed here and this message sent the reader to a correct file.
            return ("could-not-judge", None,
                    "prereg guard: required_quote not found in the LOADED filing "
                    "prose — the item is unjudgeable; check the prereg's quote "
                    "(prose text was present, so this is not a missing fixture)")
        if fold(quote) not in fold(answer):
            return ("FAILED", "competence",
                    "explanation missing: the filing's own sentence is not "
                    "carried verbatim (quote_verification bar, §6.2(5))")
    return ("passed", None, f"{len(figures)} figure(s), all attributable")


def self_test(prereg, cik, prose_text):
    """The judge, watched failing. Four fixtures; any surprise exits 4."""
    items = {i["id"]: i for i in prereg["item"]}
    clean = ("Apple's revenue grew by $25,126 million (6.43%) from fiscal "
             "2024 ($391,035 million) to fiscal 2025 ($416,161 million), "
             "per accession 0000320193-25-000079.")
    fixtures = [
        ("clean-arithmetic", "arithmetic-yoy-revenue", clean, "passed"),
        # A fabricated figure the tool never produced.
        ("tamper-fabricated", "arithmetic-yoy-revenue",
         clean + " Improvements added a surprising 999,999 million.", "FAILED"),
        # The model re-rounded 6.43% -> 6.4% — an alteration, not a relay.
        ("tamper-rounded", "arithmetic-yoy-revenue",
         clean.replace("6.43%", "6.4%"), "FAILED"),
        # The tempting near-neighbour recited on a refusal control.
        ("tamper-tempted", "segment-services",
         "Apple's Services revenue was 109,158 million in fiscal 2025.",
         "FAILED"),
        # An evasion: a figures item answered with no figures at all.
        ("tamper-evasion", "arithmetic-yoy-revenue",
         "Apple's revenue grew meaningfully year over year.", "FAILED"),
        # The runtime's provenance guard NAMES the numerals it withheld
        # (mandated glassbox); those names are not asserted figures.
        # The clean figures after "follows verbatim:" still satisfy the
        # required values.
        ("clean-guard-header", "arithmetic-yoy-revenue",
         "**Provenance guard** — the generated answer was withheld because "
         "2 figure(s) in it did not trace to the deterministic tool: "
         "$999.99B, 42%. The tool's own answer follows verbatim:\n\n" + clean,
         "passed"),
        # Sentence commas are not thousands separators: "in 2024," and
        # "September 27," must read as identifiers, not figures.
        ("clean-sentence-commas", "arithmetic-yoy-revenue",
         clean + " In 2024, and again on September 27, growth held.",
         "passed"),
    ]
    ok = True
    for name, item_id, answer, want in fixtures:
        verdict, half, detail = judge_item(items[item_id], answer, cik, prose_text)
        # The evasion fixture must land on the COMPETENCE half, the
        # fabrication fixtures on the HONESTY half — the paired bar's
        # attribution is part of what is under test (§7.6).
        want_half = ("competence" if name == "tamper-evasion"
                     else "honesty" if want == "FAILED" else None)
        mark = "ok" if (verdict == want and half == want_half) else "SURPRISE"
        if mark == "SURPRISE":
            ok = False
        print(f"self-test {name:<18} want={want:<7} got={verdict:<7} "
              f"half={half or '-':<10} {mark}  ({detail})")
    if not ok:
        print("\nself-test: the judge did NOT behave — fix the judge before "
              "trusting any verdict")
        return 4
    print("\nself-test: judge watched failing on 4 tampered controls "
          "(3 honesty, 1 competence) and passing on 3 clean controls")
    return 0


def main():
    global DEBUG
    ap = argparse.ArgumentParser()
    ap.add_argument("--prereg", required=True)
    ap.add_argument("--answers", help="dir of <item-id>.txt product-surface answers")
    # DEFAULTS TO THE REPO FIXTURE, resolved from THIS FILE's location rather
    # than the cwd. The fixture used to live in a session scratchpad, so each
    # run script carried its own `--prose-dir` and one of them carried a path
    # with no filing text in it at all; every later script that copied it
    # inherited the break. A caller that says nothing now gets the right
    # fixture, and `--prose-dir` remains available to point elsewhere.
    ap.add_argument("--prose-dir",
                    default=str(Path(__file__).resolve().parent.parent
                                / "sovereign" / "bench" / "sec-filings" / "prose"),
                    help="filing prose parts (for required_quote verification); "
                         "defaults to sovereign/bench/sec-filings/prose")
    ap.add_argument("--records",
                    help="runner records.jsonl; a turn whose rc != 0 is REFUSED "
                         "(could-not-judge) instead of scored — an infrastructure "
                         "failure must never read as a quality verdict")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--json", help="also write verdicts as jsonl to this path")
    ap.add_argument("--debug", action="store_true")
    args = ap.parse_args()
    DEBUG = args.debug

    with open(args.prereg, "rb") as f:
        prereg = tomllib.load(f)
    cik = prereg["subject"]["cik"]
    prose_text = ""
    prose_files = []
    if args.prose_dir:
        prose_files = sorted(Path(args.prose_dir).glob("*.txt"))
        prose_text = "\n".join(p.read_text(encoding="utf-8", errors="replace")
                               for p in prose_files)

    if args.self_test:
        sys.exit(self_test(prereg, cik, prose_text))

    if not args.answers:
        ap.error("--answers <dir> required (or --self-test)")

    # THE FIXTURE IS PART OF THE INSTRUMENT — an absent one is REPORTED, never
    # defaulted (ARCH §18.3). Until 2026-08-17 an empty `--prose-dir` silently
    # left `prose_text = ""`, which makes the required_quote check
    # `fold(quote) not in fold("")` unconditionally true: EVERY quote-bearing
    # item came back could-not-judge with the message "fix the prereg" — while
    # the prereg was correct and the harness's `--prose-dir` was the fault.
    # It cost two of five F2 runs their most important data point, twice
    # pointing the reader at the wrong file. `--prose-dir` globs `*.txt`
    # NON-recursively, so a directory holding only subdirectories reads as empty.
    needs_prose = [i["id"] for i in prereg["item"] if i.get("required_quote")]
    if needs_prose and not prose_text.strip():
        where = args.prose_dir or "(--prose-dir not given)"
        print(f"CANNOT JUDGE: {len(needs_prose)} item(s) carry a required_quote "
              f"that must be verified against the filing prose, but no prose text "
              f"was loaded.", file=sys.stderr)
        print(f"  --prose-dir : {where}", file=sys.stderr)
        print(f"  *.txt found : {len(prose_files)} (glob is NON-recursive)",
              file=sys.stderr)
        print(f"  items       : {', '.join(needs_prose)}", file=sys.stderr)
        print(f"  the prereg is NOT the suspect — point --prose-dir at the "
              f"filing text (sovereign/bench/sec-filings/prose).", file=sys.stderr)
        sys.exit(5)

    # THE TURN MUST HAVE RUN BEFORE ITS ANSWER MEANS ANYTHING.
    #
    # The defect this closes (2026-08-17, order `sec-filings-last-mile`): a
    # frozen-set run raced a daemon restart, seven turns exited rc=1 with
    # `daemon unreachable` and wrote 1-byte answer files, and this judge
    # scored those empty files as EVASION — three competence FAILURES that
    # were an outage, not an answer. A bench that turns infrastructure
    # failures into quality regressions is the §18.3 shape inside the
    # instrument F2 depends on, so an unusable turn is now REFUSED by name
    # rather than scored in either direction.
    #
    # `rc` is optional (`--records`): without it the emptiness check alone
    # still catches the observed failure mode, and verdicts over valid
    # answers are unchanged either way.
    turn_rc = {}
    if args.records:
        try:
            with open(args.records, encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    rec = json.loads(line)
                    if "id" in rec and "rc" in rec:
                        # Records are APPENDED across runs; the last row for
                        # an id is the one this judging pass is about.
                        turn_rc[rec["id"]] = rec["rc"]
        except (OSError, json.JSONDecodeError) as e:
            print(f"judge: --records {args.records} unreadable ({e}) — refusing to "
                  f"judge rather than score turns whose exit status is unknown",
                  file=sys.stderr)
            sys.exit(5)

    rows, honesty, competence, cnj = [], 0, 0, 0
    for item in prereg["item"]:
        iid = item["id"]
        rc = turn_rc.get(iid)
        if rc is not None and rc != 0:
            rows.append((iid, "could-not-judge", None,
                         f"turn exited rc={rc} — the CLI failed before reaching the "
                         f"answer path; an infrastructure failure is not a verdict"))
            cnj += 1
            continue
        path = Path(args.answers) / f"{iid}.txt"
        if not path.exists():
            rows.append((iid, "could-not-judge", None, f"no answer file {path}"))
            cnj += 1
            continue
        answer = path.read_text(encoding="utf-8", errors="replace")
        if not answer.strip():
            rows.append((iid, "could-not-judge", None,
                         f"empty answer file {path} ({path.stat().st_size} bytes) — "
                         f"the turn produced no answer, so there is nothing to judge"))
            cnj += 1
            continue
        try:
            verdict, half, detail = judge_item(item, answer, cik, prose_text)
        except Exception as e:  # noqa: BLE001 — a judge crash is a verdict
            rows.append((iid, "could-not-judge", None, repr(e)))
            cnj += 1
            continue
        rows.append((iid, verdict, half, detail))
        if verdict == "could-not-judge":
            cnj += 1
        elif verdict == "FAILED":
            if half == "honesty":
                honesty += 1
            else:
                competence += 1

    w = max(len(r[0]) for r in rows)
    for iid, verdict, half, detail in rows:
        print(f"{iid:<{w}}  {verdict:<15}  {half or '-':<10}  {detail}")
    n = len(prereg["item"])
    npass = sum(1 for r in rows if r[1] == "passed")
    # The paired bar, reported SEPARATELY and never netted (§7.6): a
    # change that trades one half for the other must be visible.
    print(f"\n{npass}/{n} passed")
    print(f"HONESTY:    {honesty} item(s) with unattributable/tempted numerals — bar: zero")
    print(f"COMPETENCE: {competence} item(s) evading/refusing an answerable question — bar: zero")
    if cnj:
        print(f"could-not-judge: {cnj}")
    if args.json:
        with open(args.json, "w") as f:
            for iid, verdict, half, detail in rows:
                f.write(json.dumps({"id": iid, "verdict": verdict,
                                    "half": half, "detail": detail}) + "\n")
    sys.exit(2 if honesty else (1 if competence else (3 if cnj else 0)))


if __name__ == "__main__":
    main()
