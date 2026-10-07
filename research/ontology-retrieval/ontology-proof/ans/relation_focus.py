"""Arm D for relation_ab.py: one focused call per declared relation per section, built from the
declaration alone (from/to/description), asking only for the far ends with their quotes.

The question is whether decomposition (prior art: per-relation extraction raises list recall over
joint extraction) moves recall where prompt wording did not. The `from` entities are the section's
own phase-1 entities of the declared `from` type, read from the extraction cache — what a second
pass in the pipeline would have in hand.

    python3 relation_focus.py --sections sec_00008,sec_00004,sec_00009 --runs 3
"""
import argparse, json, pathlib, sys, time, tomllib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import relation_ab as R  # noqa: E402  (same transport, scorer and attested gold)

RECIPE = pathlib.Path(__file__).resolve().parent / "recipe-dev-b.toml"
CACHE = pathlib.Path.home() / ".svrnmesh/enrichment/ft-ans-dev-b/cache/questions.json"


def declared_relation(name):
    types = tomllib.loads(RECIPE.read_text())["enrichment"]["ontology"]["types"]
    return next(t for t in types if t["name"] == name)


def from_entities(section, from_type):
    q = json.loads(CACHE.read_text())["questions_by_chapter"]
    se = next(e for e in q if e["chapter_id"] == section)["section_extraction"]
    return [e["canonical_name"] for e in se.get("entities_introduced") or [] if e.get("entity_type") == from_type]


def focused(rel, frm, user):
    system = (f"You read one section of a document and list relations of one declared type.\n\n"
              f"Relation `{rel['name']}`: from a {rel['from']} to a {rel['to']}. {rel['description']}\n\n"
              f"For the {rel['from']} named below, list EVERY {rel['to']} the section states it is in this "
              f"relation with, one item each, with the shortest quote that states it. Only what the text "
              f"states; an empty list is a correct answer.")
    schema = {"type": "object", "additionalProperties": False, "required": ["items"],
              "properties": {"items": {"type": "array", "maxItems": 40, "items": {
                  "type": "object", "additionalProperties": False, "required": ["name", "anchor"],
                  "properties": {"name": {"type": "string"}, "anchor": {"type": "string"}}}}}}
    return system, f"The {rel['from']}: {frm}\n\n{user}", schema


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sections", required=True)
    ap.add_argument("--runs", type=int, default=3)
    a = ap.parse_args()
    rel = declared_relation(R.REL)
    att, pats = R.attested_mints()
    rows = []
    for sec in a.sections.split(","):
        _, user, _ = R.dry_run(sec)
        froms = from_entities(sec, rel["from"])
        for run in range(a.runs):
            t0, items = time.time(), []
            for frm in froms:
                system, u, schema = focused(rel, frm, user)
                raw = R.call(system, u, schema)
                try:
                    items += [{"participants": [frm, i["name"]], "relation_type": R.REL} for i in json.loads(raw)["items"]]
                except (json.JSONDecodeError, KeyError):
                    pass
            s = R.score(json.dumps({"relations_introduced": items}), att[sec], pats)
            rows.append({"section": sec, "arm": "D", "run": run, "from_entities": froms,
                         "seconds": round(time.time() - t0, 1), **s})
            print(json.dumps(rows[-1]), flush=True)
    out = pathlib.Path(__file__).with_name("relation_focus_rows.json")
    out.write_text(json.dumps(rows, indent=1))
    print(f"-> {out.name}")


if __name__ == "__main__":
    main()
