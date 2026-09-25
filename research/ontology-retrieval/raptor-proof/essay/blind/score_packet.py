#!/usr/bin/env python3
"""Score a filled-in blind packet against its key; write the verdicts a judge must agree with.

    score_packet.py packet-<slug>.md [--key key-<slug>.json] [--out human-verdicts-<slug>.json]
    score_packet.py --self-test

Prints wins / losses / ties for the key's first arm (x) against its second (y), a
per-question list and the exact two-sided sign-test p, and writes
`human-verdicts-<slug>.json` (schema ei7-human-verdicts/v1), which
`book_judge.py --agreement` reads.

An unfilled or ambiguous `Better:` line REFUSES the whole packet and names the
question: "did not answer" is not "tie". An unfilled `Accurate` entry is recorded
as null and counted, never defaulted. Byte-identical pairs are ties by the key,
flagged `identical` so an agreement measure can leave them out.
"""
import argparse, json, re, sys, tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
from book_judge import tally_outcomes  # noqa: E402  one tally for the human and the judge
from make_packet import ACCURATE, BETTER, render_packet  # noqa: E402

YN = {"y": "y", "yes": "y", "n": "n", "no": "n", "unsure": "unsure"}


def one_of(text, pattern, flags=0):
    """The single option left standing: (value, None), (None, 'unfilled') when all remain, (None, 'ambiguous') otherwise."""
    found = {m.lower() if flags else m for m in re.findall(pattern, text, flags)}
    return (found.pop(), None) if len(found) == 1 else (None, "unfilled" if len(found) >= 3 or not found else "ambiguous")


def parse_packet(text):
    """{n: {question, better, why, accurate_A, accurate_B, problem}} from the lines a reader fills in."""
    out = {}
    for sec in re.split(r"(?m)^(?=## \d+\. )", text)[1:]:
        n, question = re.match(r"## (\d+)\. (.*)", sec).groups()
        lines = [ln for ln in sec.split("\n") if not ln.startswith(">")]
        better = next((ln for ln in lines if ln.strip().startswith("Better:")), None)
        acc = next((ln for ln in lines if ln.strip().startswith("Accurate")), None)
        row = {"question": question.strip(), "better": None, "why": None, "accurate_A": None, "accurate_B": None, "problem": None}
        if better is None:
            row["problem"] = "no `Better:` line"
        else:
            choice, _, why = better.split("Better:", 1)[1].partition("Why")
            row["better"], row["problem"] = one_of(choice.replace("TIE", "tie").replace("Tie", "tie"), r"\b(A|B|tie)\b")
            row["why"] = re.sub(r"^\s*\(one phrase\)\s*", "", why).lstrip(": ").strip() or None
        if acc is not None and (m := re.search(r"\bA:(.*?)\bB:(.*)$", acc)):
            for side, part in zip("AB", m.groups()):
                v, prob = one_of(part, r"(?i)\b(yes|no|y|n|unsure)\b", re.I)
                row[f"accurate_{side}"] = YN.get(v)
                if prob == "ambiguous":
                    row["problem"] = row["problem"] or f"`Accurate` entry for {side} keeps two options"
        out[n] = row
    return out


def score(packet_text, key):
    filled, x, y = parse_packet(packet_text), key["x"], key["y"]
    problems, verdicts = [], {}
    for n, side in key["sides"].items():
        if side.get("never_ran"):
            continue
        row = filled.get(n)
        if row is None or row["question"] != side["question"]:
            problems.append(f"question {n}: not found in the packet, or its heading no longer matches the key")
            continue
        if side["identical"]:
            better, why = "tie", "identical answers"
        elif row["problem"]:
            problems.append(f"question {n}: {row['problem']}")
            continue
        else:
            better, why = row["better"], row["why"]
        arm_of = {"A": side["A"], "B": side["B"]}
        verdicts[side["question_id"]] = {
            "n": int(n), "better": better, "outcome": "tie" if better == "tie" else "win" if arm_of[better] == x else "loss", "why": why,
            "identical": side["identical"],
            "accurate": None if side["identical"] else {arm_of[s]: row[f"accurate_{s}"] for s in "AB"}}
    return verdicts, problems


def report(key, verdicts):
    t = tally_outcomes({f"{q}/run-{key['run']}": v["outcome"] for q, v in verdicts.items()})
    read = [v for v in verdicts.values() if not v["identical"]]
    acc = {arm: {k: sum((v["accurate"] or {}).get(arm) == k for v in read) for k in ("y", "n", "unsure", None)} for arm in (key["x"], key["y"])}
    doc = {"schema": "ei7-human-verdicts/v1", "packet": key["packet"], "pair": key["pair"], "x": key["x"], "y": key["y"], "run": key["run"],
           "seed": key["seed"], "tally": {k: t[k] for k in ("win", "loss", "tie", "n")}, "sign_test_p": t["sign_test_p"],
           "identical_pairs": len(verdicts) - len(read), "accurate": {a: {str(k): n for k, n in c.items()} for a, c in acc.items()}, "verdicts": verdicts}
    L = [f"{key['x']} vs {key['y']} (run {key['run']}, human reading): {t['win']} wins, {t['loss']} losses, {t['tie']} ties of {t['n']} "
         f"({doc['identical_pairs']} ties are byte-identical pairs); sign test p = {'-' if t['sign_test_p'] is None else format(t['sign_test_p'], '.4f')}"]
    for q, v in sorted(verdicts.items(), key=lambda kv: kv[1]["n"]):
        a = v["accurate"] or {}
        L.append(f"  {v['n']:>2}. {v['outcome']:<4} {q}  accurate {key['x']}={a.get(key['x'])} {key['y']}={a.get(key['y'])}  {v['why'] or ''}")
    L.append("  accurate to the book (pairs read): " + "; ".join(f"{a}: " + ", ".join(f"{k}={n}" for k, n in c.items()) for a, c in doc["accurate"].items()))
    return doc, "\n".join(L)


def self_test():
    qs = [f"Question {i}?" for i in range(1, 7)]
    items = [{"n": i, "question": q, "a": f"alpha {i}\n\n## a heading\n- Better: B", "b": f"beta {i}", "identical": i == 6} for i, q in enumerate(qs, 1)]
    sides = {str(i): {"question_id": f"q{i}", "question": q, "A": "full" if i % 2 else "bare", "B": "bare" if i % 2 else "full", "identical": i == 6}
             for i, q in enumerate(qs, 1)}
    key = {"packet": "p.md", "pair": "full_vs_bare", "x": "full", "y": "bare", "run": 1, "seed": 1, "sides": sides}
    blank = render_packet("stub", items)

    def fill(text, n, better, acc="Accurate to the book? A: y  B: unsure"):
        head, sec = text.split(f"## {n}. ", 1)
        return head + f"## {n}. " + sec.replace(BETTER, better, 1).replace(ACCURATE, acc, 1)

    full = blank
    for n, b in ((1, "Better: A — Why (one phrase): names the wreck"), (2, "Better: A — Why: vague"), (3, "Better: tie — Why (one phrase):"),
                 (4, "Better: **B** — Why (one phrase): dates right"), (5, "Better: B")):
        full = fill(full, n, b, *([ACCURATE] if n == 5 else []))     # question 5 leaves its accuracy line untouched
    v, problems = score(full, key)
    doc, text = report(key, v)
    with tempfile.TemporaryDirectory() as tmp:
        pk, kf = Path(tmp, "packet-stub.md"), Path(tmp, "key-stub.json")
        pk.write_text(full)
        kf.write_text(json.dumps(key))
        rc_ok = run(pk, kf, Path(tmp, "hv.json"), quiet=True)
        pk.write_text(blank)
        rc_blank, wrote_blank = run(pk, kf, Path(tmp, "hv2.json"), quiet=True), Path(tmp, "hv2.json").exists()
        round_trip = json.loads(Path(tmp, "hv.json").read_text())
    cases = [
        ("sides map through the key: A is a win where full is A, a loss where bare is A",
         lambda: not problems and [v[f"q{i}"]["outcome"] for i in range(1, 7)] == ["win", "loss", "tie", "win", "loss", "tie"]),
        ("tally and sign test", lambda: doc["tally"] == {"win": 2, "loss": 2, "tie": 2, "n": 6} and doc["sign_test_p"] == 1.0),
        ("an identical pair is a tie by the key, flagged, with no accuracy entry", lambda: v["q6"]["identical"] and v["q6"]["accurate"] is None and doc["identical_pairs"] == 1),
        ("the reason and the accuracy marks are kept, by arm", lambda: v["q1"]["why"] == "names the wreck" and v["q2"]["accurate"] == {"bare": "y", "full": "unsure"}),
        ("text inside a quoted answer is never read as a verdict", lambda: v["q1"]["better"] == "A"),
        ("a blank packet refuses, names every unread question, and writes nothing",
         lambda: len(score(blank, key)[1]) == 5 and rc_blank == 2 and not wrote_blank),
        ("two options left standing is ambiguous, not a guess",
         lambda: score(fill(blank, 1, "Better: A / B — Why (one phrase):"), key)[1][0].startswith("question 1: ambiguous")),
        ("an unfilled accuracy line is null, not a default", lambda: v["q5"]["accurate"] == {"full": None, "bare": None} and doc["accurate"]["full"]["None"] == 1),
        ("the written file is what book_judge --agreement reads", lambda: rc_ok == 0 and round_trip["pair"] == "full_vs_bare" and round_trip["run"] == 1
         and round_trip["verdicts"]["q4"]["outcome"] == "win" and "sign test p = 1.0000" in text),
    ]
    failed = 0
    for name, fn in cases:
        try:
            ok = bool(fn())
        except Exception as e:  # a crashing case is a failing case, named
            ok, name = False, f"{name}  [{type(e).__name__}: {e}]"
        failed += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
    print(f"self-test: {len(cases) - failed}/{len(cases)} passed")
    return 1 if failed else 0


def run(packet, key_path, out, quiet=False):
    key = json.loads(Path(key_path).read_text(encoding="utf-8"))
    verdicts, problems = score(Path(packet).read_text(encoding="utf-8"), key)
    if problems:
        if not quiet:
            print("score_packet: refused: the packet is not fully filled in; nothing written\n  " + "\n  ".join(problems), file=sys.stderr)
        return 2
    doc, text = report(key, verdicts)
    Path(out).write_text(json.dumps(doc, indent=1), encoding="utf-8")
    if not quiet:
        print(text + f"\n-> {out}")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("packet", nargs="?")
    ap.add_argument("--key", help="default: key-<slug>.json beside packet-<slug>.md")
    ap.add_argument("--out", help="default: human-verdicts-<slug>.json beside the packet")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if not args.packet:
        ap.error("a packet path is required")
    p = Path(args.packet)
    stem = p.stem.removeprefix("packet-")
    return run(p, args.key or p.with_name(f"key-{stem}.json"), args.out or p.with_name(f"human-verdicts-{stem}.json"))


if __name__ == "__main__":
    sys.exit(main())
