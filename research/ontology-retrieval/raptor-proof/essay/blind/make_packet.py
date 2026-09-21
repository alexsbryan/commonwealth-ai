#!/usr/bin/env python3
"""Build a blind A/B reading packet (one pair per question) and its SEPARATE key.

    make_packet.py --runs ../../runs-essay/<slug> --bank ../bank-<slug>.toml --slug <slug> \
        [--pair full:bare] [--run 1] [--seed 20260921]

Answers are scrubbed by `essay_judge.scrub`, the judge's own scrub, so the human and
the judges read the same bytes. Sides come from `random.Random("<seed>:<x>-vs-<y>")`,
one draw per bank question in bank order (the pair is in the seed so two packets do
not share a side sequence). Byte-identical pairs are shown as such: a tie by
construction, nothing to read, and saying so names no arm.

The verification-note footer stays unless `footer_check` finds it gives the arm
away; the numbers it decided on are recorded in the key.
"""
import argparse, hashlib, json, random, re, statistics, sys, tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from essay_judge import ARMS, load_runs, route_exclusions, scrub  # noqa: E402

FOOTER = re.compile(r"\n-{3,}\s*\n\*Verification note:.*\Z", re.S)
ARM_TOKENS = re.compile(r"(?i)raptor|atlas|summar|overview")   # pipeline words; "deep" and "bare" are ordinary English in a sea novel
INSTRUCTIONS = ("For each question read Answer A and Answer B, then fill in the two lines under them: on the `Better:` line delete the "
                "options you are not choosing and add a phrase of why, and on the `Accurate` line leave one of y/n/unsure after each letter. "
                "Judge accuracy to the book first, then how fully the answer meets the question, then how well it draws on the whole book; "
                "ignore length and style. Do not open the key file until every line is filled; then run `python3 score_packet.py <this file>`.")
BETTER = "Better: A / B / tie — Why (one phrase):"
ACCURATE = "Accurate to the book? A: y/n/unsure  B: y/n/unsure"
IDENTICAL_NOTE = "Answers A and B are word-for-word identical, so this pair is a tie by construction and needs no reading."


def footer_check(runs, x, y):
    """Does the footer give the arm away? Presence, size and arm-revealing tokens, per arm, over every run."""
    stats = {}
    for arm in (x, y):
        feet = [(FOOTER.search((r.get("synth") or {}).get("answer") or "") or [""])[0] for rows in runs[arm].values() for r in rows.values()]
        have = [f for f in feet if f]
        bullets = [len(re.findall(r"^- ", f, re.M)) for f in have]
        stats[arm] = {"answers": len(feet), "with_footer": len(have), "arm_tokens_in_footer": sum(bool(ARM_TOKENS.search(f)) for f in have),
                      "bullets_mean": round(statistics.fmean(bullets), 2) if bullets else None,
                      "bullets_range": [min(bullets), max(bullets)] if bullets else None}
    rate = {a: s["with_footer"] / s["answers"] for a, s in stats.items() if s["answers"]}
    reveals = any(s["arm_tokens_in_footer"] for s in stats.values()) or (len(rate) == 2 and abs(rate[x] - rate[y]) > 0.5)
    return {"per_arm": stats, "reveals_arm": reveals, "decision": "stripped" if reveals else "kept"}


def quote(text):
    return "\n".join(">" if not ln.strip() else f"> {ln}" for ln in text.split("\n"))


def render_packet(title, items):
    """items: [{n, question, a, b, identical}] -> markdown. No arm, run or seed appears in it."""
    L = [f"# Blind reading packet: {title}", "", INSTRUCTIONS, ""]
    for it in items:
        L += [f"## {it['n']}. {it['question']}", ""]
        if it["identical"]:
            L += [IDENTICAL_NOTE, "", "Better: tie — Why (one phrase): identical answers", "", "---", ""]
            continue
        L += ["### Answer A", "", quote(it["a"]), "", "### Answer B", "", quote(it["b"]), "", BETTER, "", ACCURATE, "", "---", ""]
    return "\n".join(L)


def build(bank, runs, x, y, run_no, seed):
    corpus, excluded = bank["bank"].get("corpus", ""), route_exclusions(runs)
    rng = random.Random(f"{seed}:{x}-vs-{y}")
    strip = footer_check(runs, x, y)
    items, key = [], {}
    for n, q in enumerate(bank["questions"], 1):
        x_is_a = rng.random() < 0.5                       # drawn for every bank row, so one missing row shifts no other side
        rx, ry = runs[x][run_no].get(q["id"]), runs[y][run_no].get(q["id"])
        if q["id"] in excluded or rx is None or ry is None:
            key[str(n)] = {"question_id": q["id"], "never_ran": True}
            continue
        ans = {}
        for arm, row in ((x, rx), (y, ry)):
            raw = (row.get("synth") or {}).get("answer") or ""
            ans[arm], _, leak = scrub(FOOTER.sub("", raw) if strip["reveals_arm"] else raw, corpus)
            if leak:
                raise SystemExit(f"make_packet: refused: an arm-revealing token survives the scrub in {arm}/{q['id']}")
        a, b = (ans[x], ans[y]) if x_is_a else (ans[y], ans[x])
        items.append({"n": n, "question": q["question"], "a": a, "b": b, "identical": a == b})
        key[str(n)] = {"question_id": q["id"], "question": q["question"], "A": x if x_is_a else y, "B": y if x_is_a else x,
                       "identical": a == b, "sha256_A": hashlib.sha256(a.encode()).hexdigest()[:12], "sha256_B": hashlib.sha256(b.encode()).hexdigest()[:12]}
    return items, key, strip


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", required=True)
    ap.add_argument("--bank", required=True)
    ap.add_argument("--slug", required=True)
    ap.add_argument("--title", default=None)
    ap.add_argument("--pair", default="full:bare")
    ap.add_argument("--run", type=int, default=1)
    ap.add_argument("--seed", type=int, default=20260921)
    ap.add_argument("--out-dir", default=str(HERE))
    args = ap.parse_args()
    x, y = args.pair.split(":")
    if not {x, y} <= set(ARMS):
        ap.error(f"--pair wants arm:arm from {ARMS}")
    bank = tomllib.loads(Path(args.bank).read_text(encoding="utf-8"))
    runs, _, _ = load_runs(args.runs)
    items, key, strip = build(bank, runs, x, y, args.run, args.seed)
    suffix = "" if (x, y) == ("full", "bare") else f"-{x}-vs-{y}"
    packet, keyf = Path(args.out_dir, f"packet-{args.slug}{suffix}.md"), Path(args.out_dir, f"key-{args.slug}{suffix}.json")
    packet.write_text(render_packet(args.title or args.slug, items), encoding="utf-8")
    keyf.write_text(json.dumps({"schema": "ei7-blind-key/v1", "DO_NOT_OPEN": "until every line of the packet is filled in", "packet": packet.name,
                                "pair": f"{x}_vs_{y}", "x": x, "y": y, "run": args.run, "seed": args.seed,
                                "rng": f"random.Random('{args.seed}:{x}-vs-{y}'), one random() < 0.5 per bank question, in bank order; True puts {x} on side A",
                                "scrub": "essay_judge.scrub", "footer_check": strip, "sides": key}, indent=1), encoding="utf-8")
    ident = sum(i["identical"] for i in items)
    print(f"make_packet: {packet.name}: {len(items)} pairs ({ident} byte-identical, shown as ties), footer {strip['decision']}; key -> {keyf.name}")


if __name__ == "__main__":
    sys.exit(main())
