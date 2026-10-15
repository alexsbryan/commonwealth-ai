"""Verify probe (verify-probe/PREREG.md): each agreed act the act pass emitted, its value re-read by Choose's form from
only the lines its citation touches, in two wordings; kept when the re-read value equals the act's.

    verify_probe.py ask [leg ...]      # ask (cached by request)
    verify_probe.py gate               # 20 pairs re-asked outside the cache
    verify_probe.py score              # score.json, rows.jsonl
"""
import collections
import glob
import json
import pathlib
import random
import sys
import tomllib

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import locate_probe as L  # noqa: E402  body(), post(), answer(), plan(): the probes' one request path

RUN = ROOT / "runs/act"
OUT = ROOT / "runs/verify-probe"
DATA = HERE / "verify-probe"
LEGS = ("ours-ward", "ours-uv")
WORDINGS = {
    "V1": ("You read a passage from a document and say what it states about one field.\n\nThe user message gives the "
           "field and its declared description, the field's values each with a letter, and the passage. Answer with "
           "the letter of the value the passage states, or 0 if it states none of them.",
           "Which value does the passage state? Answer with its letter, or 0 if it states none."),
    "V2": ("Decide which declared value of a field a passage expresses.\n\nYou are given a field with its description, "
           "its possible values, lettered, and a short passage. Reply with the letter of the value the passage "
           "expresses, or 0 if it expresses none of them.",
           "Which value does the passage express? Reply with its letter, or 0 if none."),
}


def leg_parts(leg):
    """(pairs, {doc: reader lines}, field) for one leg: the act kind's protocol field and its declared values."""
    import lance  # noqa: PLC0415
    pairs = [json.loads(l) for l in open(RUN / leg / "act-pairs.jsonl")]
    rec = tomllib.loads(pathlib.Path(glob.glob(str(RUN / leg / "data/recipes/*/recipe.toml"))[0]).read_text())
    kinds, _ = L.plan(rec)
    values = {p["value"] for p in pairs}
    field = next(a for k in kinds for a in k["fields"] if values <= set(a["values"]))
    idx = next(pathlib.Path(p) for p in glob.glob(str(RUN / leg / "data/indexes/*/chunks.lance")))
    by = collections.defaultdict(list)
    for r in lance.dataset(str(idx)).to_table(columns=["id", "source_doc_id", "content"]).to_pylist():
        by[r["source_doc_id"]].append(r)
    lines = {}
    for d, ch in by.items():
        ch.sort(key=lambda r: int(r["id"]))
        lines[d] = [ln.rstrip() for ln in "\n".join(c["content"] for c in ch).split("\n") if ln.strip()]
    return pairs, lines, field


def question(field, passage, wording):
    sysp, q = WORDINGS[wording]
    labels = [L.LABELS[i] for i in range(len(field["values"]))]
    opts = "".join(f"{lab} {v}\n" for lab, v in zip(labels, field["values"]))
    user = (f"Field: {field['name']} ({field.get('description', '')})\nValues:\n{opts}{L.NONE} not stated\n\n"
            f"Passage:\n<<<\n{passage}\n>>>\n\n{q}")
    return sysp, user, labels + [L.NONE], dict(zip(labels, field["values"]))


def ask_one(field, passage, wording, root=OUT):
    sysp, user, labels, meaning = question(field, passage, wording)
    d = L.answer(L.post(L.body(sysp, user, labels), root=root), labels)
    if d is None:
        return None, None
    top = max(d, key=d.get)
    return meaning.get(top, "not stated"), d


def passage_of(pair, lines):
    """The cited lines. A pair whose lines the leg's scorer could not map ('?') is judged only when its document is
    one line (cited by code, so that line is the citation); otherwise None, could-not-judge."""
    raw = lines.get(pair["document"], [])
    if not isinstance(pair["first_line"], int):
        return raw[0] if len(raw) == 1 else None
    return "\n".join(raw[n - 1] for n in range(pair["first_line"], pair["last_line"] + 1) if 0 < n <= len(raw))


def cmd_ask(legs):
    for leg in legs:
        pairs, lines, field = leg_parts(leg)
        n = 0
        for p in pairs:
            text = passage_of(p, lines)
            for w in WORDINGS if text else ():
                ask_one(field, text, w)
                n += 1
        print(f"{leg}: {len(pairs)} pairs, {n} questions", flush=True)


def cmd_gate():
    items = []
    for leg in LEGS:
        if (RUN / leg / "act-pairs.jsonl").exists():
            pairs, lines, field = leg_parts(leg)
            items += [(field, t) for p in pairs if (t := passage_of(p, lines))]
    sample = random.Random(L.SEED).sample(items, min(20, len(items)))
    same = sum(ask_one(f, t, "V1")[0] == ask_one(f, t, "V1", root=OUT / "regate")[0] for f, t in sample)
    res = {"re_asked": len(sample), "same": same, "passed": same >= 19}
    print(json.dumps(res))
    DATA.mkdir(exist_ok=True)
    (DATA / "gate.json").write_text(json.dumps(res, indent=1) + "\n")


def cmd_score():
    report, rows = {}, []
    for leg in LEGS:
        if not (RUN / leg / "act-pairs.jsonl").exists():
            report[leg] = "never-ran (no act-pairs.jsonl)"
            continue
        pairs, lines, field = leg_parts(leg)
        a = collections.Counter()
        spread = collections.defaultdict(collections.Counter)
        for p in pairs:
            text = passage_of(p, lines)
            if not text:
                a["could_not_judge"] += 1
                continue
            got = {w: ask_one(field, text, w)[0] for w in WORDINGS}
            kept = {w: got[w] == p["value"] for w in WORDINGS}
            both = all(kept.values())
            cls = "true" if p["right"] else ("false_no_particular" if not p.get("gold") else "false")
            a[cls] += 1
            for w in WORDINGS:
                a[f"{cls}_kept_{w}"] += kept[w]
            a[f"{cls}_kept_both"] += both
            spread[cls][got["V1"]] += 1
            rows.append({"leg": leg, "document": p["document"], "value": p["value"], "gold": p.get("gold"),
                         "right": p["right"], "class": cls, "reread": got, "kept_both": both, "passage": text[:300]})
        res = {"could_not_judge": a["could_not_judge"]}
        for cls in ("true", "false", "false_no_particular"):
            n = a[cls]
            if not n:
                continue
            res[cls] = {"pairs": n, **{f"kept_{w}": a[f"{cls}_kept_{w}"] for w in (*WORDINGS, "both")},
                        "reread_V1": dict(spread[cls])}
        t, f = a["true"], a["false"] + a["false_no_particular"]
        kept_true = a["true_kept_both"] / t if t else None
        rej_false = 1 - (a["false_kept_both"] + a["false_no_particular_kept_both"]) / f if f else None
        res["bar"] = {"true_kept": round(kept_true, 3) if kept_true is not None else None,
                      "false_rejected": round(rej_false, 3) if rej_false is not None else None,
                      "passed": bool(kept_true is not None and rej_false is not None and kept_true >= .8 and rej_false >= .5)}
        report[leg] = res
        print(json.dumps({leg: res}, indent=1), flush=True)
    DATA.mkdir(exist_ok=True)
    (DATA / "score.json").write_text(json.dumps(report, indent=1) + "\n")
    (DATA / "rows.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "score"
    if cmd == "ask":
        cmd_ask(sys.argv[2:] or [leg for leg in LEGS if (RUN / leg / "act-pairs.jsonl").exists()])
    elif cmd == "gate":
        cmd_gate()
    else:
        cmd_score()
