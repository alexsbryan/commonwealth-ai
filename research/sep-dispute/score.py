#!/usr/bin/env python3
"""Code-checkable scorer for the SEP dispute bank (bank.toml; the rule and the five checks are in
its header). Model-free: regexes over the answer text, sentence windows for holder credit.

    python3 score.py --selftest                     # each check watched failing on a known-bad answer
    python3 score.py --control                      # positive control: the witnesses read as answers
    python3 score.py answers.json [-o scores.json]  # answers.json: {"<item id>": "<answer text>"}

Verdicts per item: passed / failed / could-not-judge (no answer or empty answer; "did not
answer" is not "answered wrong"). Neutral items are reported as `neutral`, never scored.
census.py imports norm / hits / sentences from here, so the census and the scorer read text the
same way.
"""
import argparse, json, pathlib, re, sys, tomllib, unicodedata

HERE = pathlib.Path(__file__).resolve().parent
BANK = HERE / "bank.toml"
_TRANS = str.maketrans({"“": '"', "”": '"', "‘": "'", "’": "'",
                        "–": "-", "—": "-", " ": " "})


def norm(s):
    return re.sub(r"\s+", " ", unicodedata.normalize("NFKC", s or "").translate(_TRANS)).strip()


def hits(patterns, text, case_sensitive=False):
    flags = 0 if case_sensitive else re.IGNORECASE
    return any(re.search(p, text, flags) for p in patterns)


def sentences(text, split):
    return [s for s in re.split(split, norm(text)) if s.strip()]


def load_bank(path=BANK):
    with open(path, "rb") as f:
        return tomllib.load(f)


def score_item(item, answer, scoring):
    if item.get("neutral"):
        return {"verdict": "neutral", "why": item.get("neutral_why", "")}
    if answer is None or not norm(answer):
        return {"verdict": "could-not-judge", "why": "no answer"}
    text, sents = norm(answer), sentences(answer, scoring["sentence_split"])
    positions = item["positions"]
    required = [p for p in positions if p.get("required", True)]
    neutral_pats = [pat for h in item.get("neutral_holders", []) for pat in h["match"]]

    named = {p["id"]: hits(p["match"], text) for p in required}

    def gold_pats(p):
        return [pat for h in p.get("holders", []) for pat in h["match"]]

    credited = {}
    for p in required:
        # Window: a sentence and the one after it, so "Robinson raises an objection. He argues ..."
        # credits Robinson. Misattribution stays sentence-level.
        wins = [" ".join(sents[i:i + 2]) for i in range(len(sents))]
        credited[p["id"]] = any(hits(p["match"], w) and hits(gold_pats(p), w, True) for w in wins)

    # Per person pattern: the positions of this item it is gold for. A sentence that names such a
    # person and some position, but none of the person's own, misattributes.
    owner = {}
    for p in positions:
        for h in p.get("holders", []):
            for pat in h["match"]:
                owner.setdefault(pat, (h["name"], set()))[1].add(p["id"])
    # Exempt: the sentence also names a gold holder of a position it names ("Robinson objects to
    # Allison's epistemic condition"), or carries an opposition cue ("X rejects P"). Conservative:
    # a swap phrased as opposition is missed, a plain swap ("Adickes holds the two-object reading")
    # is caught.
    misattributed = []
    for s in sents:
        named_here = {p["id"] for p in positions if hits(p["match"], s)}
        # A position that is an objection TO someone names its target ("Allison appears to reverse
        # this relation"): the target's mention there is neither credit nor misattribution.
        targets = [t for p in positions if p["id"] in named_here for t in p.get("targets", [])]
        credited_here = {pid for pat, (_, owns) in owner.items() if re.search(pat, s) for pid in owns} & named_here
        cue = re.search(scoring["opposition_cue"], s, re.I)
        for pat, (hname, owns) in owner.items():
            if pat in neutral_pats or pat in targets or not re.search(pat, s) or not named_here or named_here & owns:
                continue
            if cue or credited_here:
                continue
            misattributed.append({"sentence": s, "holder": hname, "named_positions": sorted(named_here),
                                  "holder_owns": sorted(owns)})

    collapse_hits = [pat for pat in scoring["collapse"] + item.get("collapse", []) if re.search(pat, text, re.I)]
    collapsed = not all(named.values()) or bool(collapse_hits)

    forbidden_hits = [pat for pat in item.get("forbidden", []) if re.search(pat, text)]
    hedged = (not item.get("hedge_required")) or (hits(scoring["hedge_markers"], text) and not forbidden_hits)

    checks = {
        "positions_named": all(named.values()),
        "holders_credited": all(credited.values()),
        "no_misattribution": not misattributed,
        "no_collapse": not collapsed,
        "hedge_kept": hedged,
    }
    return {"verdict": "passed" if all(checks.values()) else "failed", "checks": checks,
            "named": named, "credited": credited, "misattributed": misattributed,
            "collapse_hits": collapse_hits, "forbidden_hits": forbidden_hits}


def score_all(bank, answers):
    return {it["id"]: score_item(it, answers.get(it["id"]), bank["scoring"]) for it in bank["questions"]}


# Known answers, written before the scorer ran. Each bad answer must fail exactly the check(s)
# named (a missing position fails three by construction); the good answer must pass. A check that cannot be made red here is not a check.
SELFTEST = {
    "d03-one-object-vs-two-object": [
        ("good", "passed", None,
         "One-object readers such as Adickes, Allais and Westphal hold that appearances are numerically identical "
         "to things in themselves. Two-object (non-identity) readers deny this; Aquila and Van Cleve hold a "
         "non-identity version of phenomenalism, and Stang argues there is theoretical warrant for non-identity. "
         "Whether such identity claims have content outside practical contexts is controversial."),
        ("one-sided", "failed", ["positions_named", "holders_credited", "no_collapse"],
         "Adickes, Allais and Westphal are one-object readers: appearances are numerically identical to things in "
         "themselves. This is controversial."),
        ("swapped", "failed", "no_misattribution",
         "Adickes holds the two-object reading. One-object readers such as Allais and Westphal hold that "
         "appearances are identical to things in themselves; Van Cleve also defends non-identity. "
         "It is controversial."),
        ("no-holder", "failed", "holders_credited",
         "On the one-object reading appearances are identical to things in themselves; on the two-object reading "
         "they are not. Scholars such as Adickes and Van Cleve have written on this. It is controversial."),
        ("unhedged", "failed", "hedge_kept",
         "One-object readers such as Adickes, Allais and Westphal hold that appearances are identical to things in "
         "themselves. Two-object readers deny this; Aquila and Van Cleve hold the non-identity version."),
        ("settled", "failed", "no_collapse",
         "One-object readers such as Adickes hold identity. Two-object readers such as Van Cleve deny it. "
         "Scholars now agree that the two-object reading is right, though this may be debated."),
    ],
    "d07-robinson-vs-allison": [
        ("good", "passed", None,
         "For Allison an epistemic condition is a representation we must apply to objects to cognize them. "
         "Robinson objects that the qualification 'qua appearances' is otiose, so the view collapses into "
         "phenomenalism or misrepresentation. The entry suggests Allison might reply that the objection assumes "
         "transcendental realism."),
        ("pronoun", "passed", None,
         "Allison treats space and time as epistemic conditions. Robinson raises a general objection to that "
         "notion. He argues that the qualification 'qua appearances' is otiose, so the view becomes phenomenalism "
         "or misrepresentation. Allison might reply that this assumes transcendental realism."),
        ("objects-to", "passed", None,
         "Allison's epistemic condition is a representation we must apply to objects to cognize them. "
         "Robinson objects to Allison's notion of an epistemic condition: the qualification 'qua appearances' is "
         "otiose. Allison might reply that this assumes transcendental realism."),
        # Since the two-sentence credit window, holders_credited no longer catches this swap (Robinson
        # sits next to the otiose sentence); misattribution still does.
        ("swapped", "failed", "no_misattribution",
         "Robinson holds that an epistemic condition is a representation we must apply to objects. "
         "Allison argues the qualification 'qua appearances' is otiose. Robinson might reply otherwise."),
        ("reply-as-fact", "failed", "hedge_kept",
         "For Allison an epistemic condition is a representation we must apply to objects to cognize them. "
         "Robinson objects that the qualification 'qua appearances' is otiose. Allison replies that the objection "
         "assumes transcendental realism, though it might be unclear."),
    ],
}


def selftest(bank):
    items = {it["id"]: it for it in bank["questions"]}
    bad = 0
    for iid, cases in SELFTEST.items():
        for name, want, failing, answer in cases:
            r = score_item(items[iid], answer, bank["scoring"])
            failed_checks = [k for k, v in r["checks"].items() if not v]
            # The bad answer must fail the named check and only it, or the case does not isolate it.
            want_failed = [] if failing is None else ([failing] if isinstance(failing, str) else failing)
            ok = r["verdict"] == want and failed_checks == want_failed
            bad += not ok
            print(f"{'ok  ' if ok else 'FAIL'} {iid:34} {name:14} want={want:6} got={r['verdict']:6} "
                  f"failing={failed_checks}")
    r = score_item(items["d01-phenomenalist-vs-dual-aspect"], "", bank["scoring"])
    ok = r["verdict"] == "could-not-judge"
    bad += not ok
    print(f"{'ok  ' if ok else 'FAIL'} empty answer -> {r['verdict']}")
    r = score_item(items["n01-a-vs-b-edition"], "anything", bank["scoring"])
    ok = r["verdict"] == "neutral"
    bad += not ok
    print(f"{'ok  ' if ok else 'FAIL'} neutral item -> {r['verdict']}")
    return bad


def control(bank):
    """Positive control: each item's own witness quotes, joined, read as an answer. The text that
    defines the gold should pass; a failure here is an instrument defect or a phrasing the
    window cannot see, and is reported, not hidden."""
    passed = 0
    for it in bank["questions"]:
        if it.get("neutral"):
            continue
        parts = [w["quote"] for w in it.get("hedges", [])]
        for p in it["positions"]:
            parts += [w["quote"] for w in p["witnesses"]]
            parts += [w["quote"] for h in p.get("holders", []) for w in h["witnesses"]]
        ans = " ".join(q if q.rstrip()[-1:] in ".;" else q + "." for q in parts)  # quotes stay separate sentences
        r = score_item(it, ans, bank["scoring"])
        passed += r["verdict"] == "passed"
        print(f'{it["id"]:42} {r["verdict"]:7} {[k for k, v in r["checks"].items() if not v]}')
    print(f"control: {passed} passed")
    return passed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("answers", nargs="?")
    ap.add_argument("--selftest", action="store_true")
    ap.add_argument("--control", action="store_true")
    ap.add_argument("-o", "--out")
    a = ap.parse_args()
    bank = load_bank()
    if a.selftest:
        sys.exit(1 if selftest(bank) else 0)
    if a.control:
        control(bank)
        return
    if not a.answers:
        ap.error("answers.json or --selftest")
    res = score_all(bank, json.loads(pathlib.Path(a.answers).read_text()))
    out = json.dumps(res, indent=1, ensure_ascii=False)
    if a.out:
        pathlib.Path(a.out).write_text(out + "\n")
    tally = {}
    for r in res.values():
        tally[r["verdict"]] = tally.get(r["verdict"], 0) + 1
    print(json.dumps(tally))


if __name__ == "__main__":
    main()
