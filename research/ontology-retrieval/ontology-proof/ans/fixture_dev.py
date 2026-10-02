#!/usr/bin/env python3
"""O-T1 fixture (feature-fidelity): the paragraphs that anchor the DEV K1 hoards, as one markdown file.

    fixture_dev.py [--out text-dev/ans-dev.md]

THE SPLIT, fixed 2026-10-02 before any arm ran on it: a K1 row is DEV when its
IGCH number is even, HOLDOUT when odd. Tuning reads dev rows only; holdout rows
are read once, at O-T2. (The O-T0 census before the split printed every row's
as-built recall; no decision has used a holdout row.)

Which paragraphs: exactly make_bank's own `hoard_paragraphs` for each dev hoard
— the anchoring paragraphs plus the section each anchoring heading opens — so
the fixture holds the text the bank's `members_local` was counted on, no more.
Each kept paragraph is emitted under its nearest heading, and headings carry
the full breadcrumb, so a section's title in the fixture equals its title in
the full corpus and the 3c section-context pass sees the same segments.

THE ARMS: `recipe-dev-a.toml` and `recipe-dev-b.toml` are written beside the
fixture, DERIVED from `recipe.toml` — id, name and path swapped, nothing else
— so the study's ontology has one copy. Arm B adds exactly ARM_B_RELATION.
"""
import argparse, json, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import make_bank  # noqa: E402  (its paragraphs() / hoard_paragraphs(), not a second reading)


ARM_B_RELATION = '''# feature-fidelity O-T1 arm B: membership as a relation the extractor can emit from
# list prose ("staters of Abydus, Lampsacus and Miletus"), where study 1 could only
# carry it as a `hoard` attribute on a coin entity the prose rarely produces.
[[enrichment.ontology.types]]
name = "holds_coins_of"
kind = "relation"
from = "hoard"
to = "mint"
description = "The hoard contains coins struck at this mint. A hoard report lists its coins mint by mint; every mint named for a hoard is one of these, even when no single coin is described."

'''


def arm_recipes(fixture):
    """recipe.toml -> the two arm recipes over `fixture`, written beside it."""
    base = (HERE / "recipe.toml").read_text(encoding="utf8")
    anchor = '[[enrichment.ontology.types]]\nname = "attribution"'
    assert anchor in base, "recipe.toml no longer declares attribution where arm B inserts"
    for arm, extra in (("a", ""), ("b", ARM_B_RELATION)):
        text = re.sub(r'^id = "ei7-ans"$', f'id = "ft-ans-dev-{arm}"', base, count=1, flags=re.M)
        text = re.sub(r'^name = ".*"$', f'name = "feature-fidelity O-T1 arm {arm.upper()} over the dev hoard entries"',
                      text, count=1, flags=re.M)
        text = re.sub(r'^path = ".*"$', f'path = "{fixture}"', text, count=1, flags=re.M)
        text = text.replace(anchor, extra + anchor)
        (fixture.parent / f"recipe-dev-{arm}.toml").write_text(text, encoding="utf8")


def dev(igch):
    return igch % 2 == 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", type=pathlib.Path, default=HERE / "text-dev/ans-dev.md")
    ap.add_argument("--entries-only", action="store_true",
                    help="keep a paragraph only under a heading whose LAST segment names the hoard "
                         "(the hoard's own entry), not the long sections an anchor opens")
    a = ap.parse_args()
    paras = make_bank.paragraphs()
    hoards = {h["igch"]: h for h in json.loads((HERE / "truth/hoards.json").read_text())["hoards"]}
    rows = [json.loads(p.read_text()) for p in sorted((HERE / "gold").glob("list-igch*.json"))]
    dev_rows = [r for r in rows if dev(r["igch"])]
    def heading_of(i):
        if paras[i][3]:
            return i
        return next((j for j in range(i - 1, -1, -1) if paras[j][3] and paras[j][0] == paras[i][0]), None)

    keep, local = set(), {}
    for r in dev_rows:
        hp = make_bank.hoard_paragraphs(hoards[r["igch"]], r["name"], paras)
        if a.entries_only:
            pat = make_bank.rx(r["name"])
            hp = [i for i in hp if (h := heading_of(i)) is not None
                  and pat.search(make_bank.fold(paras[h][4].rsplit(" › ", 1)[-1]))]
        keep |= set(hp)
        # The ceiling THIS fixture can support: gold members its kept text names.
        text = " ".join(paras[i][2] for i in hp)
        local[r["id"]] = [m for m in r["members"] if make_bank.rx(make_bank.short(m)).search(text)]

    out, open_heading, sections = [], None, 0
    for i in sorted(keep):
        level, raw = paras[i][3], paras[i][4]
        heading = heading_of(i)
        if heading is not None and heading != open_heading:
            out.append(paras[heading][4]); open_heading = heading; sections += 1
        if not level:
            out.append(raw)
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text("\n\n".join(out) + "\n", encoding="utf8")
    arm_recipes(a.out.resolve())
    ceiling = a.out.with_suffix(".local.json")
    ceiling.write_text(json.dumps({k: v for k, v in local.items() if v}, indent=1))
    words = sum(len(b.split()) for b in out)
    print(f"dev rows {len(dev_rows)} (holdout {len(rows) - len(dev_rows)}): "
          f"{len(keep)} paragraphs, {sections} sections, {words} words -> {a.out}")
    print(f"rows with >=1 member in the fixture text: {sum(1 for v in local.values() if v)}"
          f" ({sum(len(v) for v in local.values())} members) -> {ceiling}")


if __name__ == "__main__":
    main()
