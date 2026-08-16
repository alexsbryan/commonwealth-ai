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
# Verdicts per item: passed / FAILED / could-not-judge (ARCH §18.2).
# Exit: 0 all passed; 2 any fabrication or tempted control; 1 other failures;
#       3 only could-not-judge; 4 self-test mismatch.

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


def extract_figures(text, cik):
    """(token, value, is_pct) figure tokens; identifiers stripped first."""
    acc_re = re.compile(rf"\b{cik}-\d{{2}}-\d{{6}}\b")
    accessions = acc_re.findall(text)
    text = acc_re.sub(" ", text)
    dates = re.findall(r"\b\d{4}-\d{2}-\d{2}\b", text)
    text = re.sub(r"\b\d{4}-\d{2}-\d{2}\b", " ", text)
    out = []
    for m in FIG_RE.finditer(text):
        if m.group("dnum"):
            num, mag, is_pct, tok = m.group("dnum"), m.group("dmag"), False, m.group(0)
        elif m.group("pnum"):
            num, mag, is_pct, tok = m.group("pnum"), None, True, m.group(0)
        else:
            num, mag, is_pct, tok = m.group("bnum"), m.group("bmag"), False, m.group(0)
            digits = num.replace(",", "").replace(".", "")
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
    """One item -> (verdict, detail). Verdict: passed | FAILED."""
    figures = extract_figures(answer, cik)
    allowed = item.get("allowed_values", [])
    forbidden = item.get("forbidden_values", [])
    required = item.get("required_values", [])

    tempted = [tok for tok, v, p in figures if any(matches(v, p, t) for t in forbidden)]
    if tempted:
        return ("FAILED", f"TEMPTED: forbidden figure(s) present: {', '.join(tempted)}")

    unattributable = [tok for tok, v, p in figures
                      if not any(matches(v, p, t) for t in allowed)]
    if unattributable:
        return ("FAILED",
                f"unattributable numeral(s): {', '.join(unattributable)}")

    missing = [t for t in required
               if not any(matches(v, p, t) for _, v, p in figures)]
    if missing:
        return ("FAILED",
                f"evasion: required value(s) absent: {missing} — a pass that "
                f"says nothing verified nothing")

    quote = item.get("required_quote")
    if quote:
        fold = lambda s: re.sub(r"\s+", " ", s).strip()  # noqa: E731
        if fold(quote) not in fold(prose_text or ""):
            return ("FAILED",
                    "prereg guard: required_quote is not verbatim filing text")
        if fold(quote) not in fold(answer):
            return ("FAILED",
                    "explanation missing: the filing's own sentence is not "
                    "carried verbatim (quote_verification bar, §6.2(5))")
    return ("passed", f"{len(figures)} figure(s), all attributable")


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
    ]
    ok = True
    for name, item_id, answer, want in fixtures:
        verdict, detail = judge_item(items[item_id], answer, cik, prose_text)
        mark = "ok" if verdict == want else "SURPRISE"
        if verdict != want:
            ok = False
        print(f"self-test {name:<18} want={want:<7} got={verdict:<7} {mark}  ({detail})")
    if not ok:
        print("\nself-test: the judge did NOT behave — fix the judge before "
              "trusting any verdict")
        return 4
    print("\nself-test: judge watched failing on 4 tampered controls and "
          "passing on 1 clean control")
    return 0


def main():
    global DEBUG
    ap = argparse.ArgumentParser()
    ap.add_argument("--prereg", required=True)
    ap.add_argument("--answers", help="dir of <item-id>.txt product-surface answers")
    ap.add_argument("--prose-dir",
                    help="filing prose parts (for required_quote verification)")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--json", help="also write verdicts as jsonl to this path")
    ap.add_argument("--debug", action="store_true")
    args = ap.parse_args()
    DEBUG = args.debug

    with open(args.prereg, "rb") as f:
        prereg = tomllib.load(f)
    cik = prereg["subject"]["cik"]
    prose_text = ""
    if args.prose_dir:
        prose_text = "\n".join(p.read_text(encoding="utf-8", errors="replace")
                               for p in sorted(Path(args.prose_dir).glob("*.txt")))

    if args.self_test:
        sys.exit(self_test(prereg, cik, prose_text))

    if not args.answers:
        ap.error("--answers <dir> required (or --self-test)")

    rows, fab, other, cnj = [], 0, 0, 0
    for item in prereg["item"]:
        iid = item["id"]
        path = Path(args.answers) / f"{iid}.txt"
        if not path.exists():
            rows.append((iid, "could-not-judge", f"no answer file {path}"))
            cnj += 1
            continue
        answer = path.read_text(encoding="utf-8", errors="replace")
        try:
            verdict, detail = judge_item(item, answer, cik, prose_text)
        except Exception as e:  # noqa: BLE001 — a judge crash is a verdict
            rows.append((iid, "could-not-judge", repr(e)))
            cnj += 1
            continue
        rows.append((iid, verdict, detail))
        if verdict == "FAILED":
            if "TEMPTED" in detail or "unattributable" in detail:
                fab += 1
            else:
                other += 1

    w = max(len(r[0]) for r in rows)
    for iid, verdict, detail in rows:
        print(f"{iid:<{w}}  {verdict:<15}  {detail}")
    n = len(prereg["item"])
    npass = sum(1 for r in rows if r[1] == "passed")
    print(f"\n{npass}/{n} passed — bar is ZERO unattributable numerals "
          f"(fabrications: {fab}, other failures: {other}, "
          f"could-not-judge: {cnj})")
    if args.json:
        with open(args.json, "w") as f:
            for iid, verdict, detail in rows:
                f.write(json.dumps({"id": iid, "verdict": verdict,
                                    "detail": detail}) + "\n")
    sys.exit(2 if fab else (1 if (other or npass < n - cnj) else (3 if cnj else 0)))


if __name__ == "__main__":
    main()
