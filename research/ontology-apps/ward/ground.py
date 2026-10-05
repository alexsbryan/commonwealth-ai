#!/usr/bin/env python3
"""The reader's verifier gate: a value the model read is kept only if the sentences it cites show it.

    ground.py --atoms <stage or focus atoms.json> --out <dir> [--type deal_mention]

The model proposes a mention; it is not the authority on what the message says. Each READ attribute
(a span of the text: delivery point, period, price, deal reference) is checked in code against the
mention's own cited sentences (its anchor) and dropped when they do not show it. Classifications of a
passage (commodity, kind, stage) and registry refs (counterparty, via: already held to the parties the
message touches by the reader's schema) are not quotations and are not gated. Every drop is counted per
field in <out>/ground_report.json; nothing is rewritten, only removed.
"""
import argparse, collections, json, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compose as K  # noqa: E402

MONTHS = ("jan feb mar apr may jun jul aug sep sept oct nov dec january february march april june july "
          "august september october november december").split()
TIME_WORD = re.compile(r"\b(" + "|".join(MONTHS) + r"|q[1-4]|\d{4}|\d{1,2}/\d{1,2}(/\d{2,4})?|year|years|month|months|"
                       r"winter|summer|spring|fall|term|daily|weekly|monthly|annual)\b")
# Words that name the kind of thing, not which one: a delivery point is shown by its proper part.
GENERIC = {"the", "at", "of", "and", "point", "points", "delivery", "receipt", "plant", "site", "station", "hub",
           "pipeline", "system", "city", "gate", "citygate", "border", "interconnect", "meter", "zone", "area"}


def tokens(s):
    return [t for t in K.fold(s).split() if len(t) >= 3 or t.isdigit()]


def shown(value, quote):
    """A read text value is shown when every distinctive token of it occurs in the cited text, spaces
    ignored (City Gate / Citygate)."""
    q, qs = f" {K.fold(quote)} ", K.fold(quote).replace(" ", "")
    want = [t for t in tokens(value) if t not in GENERIC] or tokens(value)
    return bool(want) and all(f" {t} " in q or t in qs for t in want)


def number_shown(value, quote):
    nums = re.findall(r"\d+(?:\.\d+)?", str(value))
    return bool(nums) and any(re.search(rf"(?<![\d.]){re.escape(n)}(?![\d])", quote) for n in nums)


def ground(attrs, quote):
    """-> (kept attrs, [dropped field names]). Pure: same mention, same verdict."""
    out, dropped = dict(attrs), []
    if out.get("delivery_point") and not shown(out["delivery_point"], quote):
        dropped.append("delivery_point"); out["delivery_point"] = None
    if (out.get("period_start") or out.get("period_end")) and not TIME_WORD.search(K.fold(quote)):
        dropped.append("period"); out["period_start"] = out["period_end"] = None
    if out.get("price") and not number_shown(out["price"], quote):
        dropped.append("price"); out["price"] = None
    if out.get("deal_ref"):
        ok = number_shown(out["deal_ref"], quote) if re.search(r"\d", str(out["deal_ref"])) else shown(out["deal_ref"], quote)
        if not ok:
            dropped.append("deal_ref"); out["deal_ref"] = None
    return out, dropped


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--atoms", type=pathlib.Path, required=True)
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--type", default="deal_mention")
    a = ap.parse_args()
    atoms = json.loads(a.atoms.read_text())["atoms"]
    rep = collections.Counter()
    for x in atoms:
        d = x["data"]
        if x["atom_type"] != "Claim" or d.get("claim_kind") != a.type:
            continue
        rep["mentions"] += 1
        for f in ("delivery_point", "price", "deal_ref"):
            rep[f"{f}: read"] += bool((d.get("attributes") or {}).get(f))
        rep["period: read"] += bool((d.get("attributes") or {}).get("period_start") or (d.get("attributes") or {}).get("period_end"))
        d["attributes"], dropped = ground(d.get("attributes") or {}, d.get("anchor") or "")
        for f in dropped:
            rep[f"{f}: not shown by its cited sentences, dropped"] += 1
        if dropped:
            d["ungrounded"] = dropped
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "atoms.json").write_text(json.dumps({"schema_version": "prototype", "atoms": atoms}))
    (a.out / "ground_report.json").write_text(json.dumps(dict(sorted(rep.items())), indent=1) + "\n")
    print(json.dumps(dict(sorted(rep.items())), indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
