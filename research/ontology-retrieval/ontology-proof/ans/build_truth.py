#!/usr/bin/env python3
"""Held-out truth for the hoards truth/hoard_refs.json names: each hoard's CoinHoards/IGCH
NUDS record (raw/coinhoards, github.com/AmericanNumismaticSociety/coinhoards, ODbL) read into
three member lists, labelled by nomisma.org's English prefLabel (CC BY), fetched once per id from nomisma's data repo.

  mints          nuds:geogname role="mint"       (role="region" is kept apart: not a mint)
  rulers         nuds:persname role="authority"  (corpname/famname kept apart: not a person)
  denominations  nuds:denomination
A member whose EVERY occurrence in the record is marked nomisma `uncertain_value` goes to
`uncertain`, not to the list. Nothing here reads the monographs.

    python3 build_truth.py      # writes truth/hoards.json, truth/labels.json
"""
import json, pathlib, re, sys
import xml.etree.ElementTree as ET

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fetch  # noqa: E402

MIRROR = "https://raw.githubusercontent.com/nomisma/data/master/id/{}.rdf"
N, X = "{http://nomisma.org/nuds}", "{http://www.w3.org/1999/xlink}"
H = "{http://nomisma.org/nudsHoard}"
KINDS = {"mints": ("geogname", "mint"), "rulers": ("persname", "authority"), "denominations": ("denomination", None)}
APART = {"regions": ("geogname", "region"), "corporate_authorities": ("corpname", None), "families": ("famname", None)}


def label(uri, inline, labels):
    if uri not in labels:
        # nomisma.org itself timed out (2 of 3 requests, 2026-09-20); github.com/nomisma/data is
        # its published dataset, one RDF file per id, and is what this reads.
        body = fetch.get(MIRROR.format(uri.rsplit("/", 1)[1]))
        m = re.search(r'<skos:prefLabel xml:lang="en">([^<]+)</skos:prefLabel>', body.decode("utf8")) if body else None
        en = m.group(1).replace("&amp;", "&") if m else None
        labels[uri] = {"label": en or inline, "source": "nomisma" if en else "nuds-inline (nomisma fetch gave no en label)",
                       "nuds_inline": inline}
    return labels[uri]["label"]


def members(root, tag, role, labels):
    seen = {}
    for el in root.iter(N + tag):
        uri = el.get(X + "href")
        if uri and "nomisma.org/id/" in uri and (role is None or el.get(X + "role") == role):
            seen.setdefault(uri, []).append(el.get("certainty") is None)
    sure = sorted({label(u, None, labels) for u, c in seen.items() if any(c)})
    return sure, sorted({label(u, None, labels) for u, c in seen.items() if not any(c)})


def main():
    refs = json.loads((HERE / "truth/hoard_refs.json").read_text())
    lp = HERE / "truth/labels.json"
    labels = json.loads(lp.read_text()) if lp.exists() else {}
    hoards = []
    for hid, works in refs.items():
        root = ET.parse(HERE / f"raw/coinhoards/nuds/{hid}.xml").getroot()
        for el in root.iter():                   # remember each id's inline label as the fallback
            if (u := el.get(X + "href")) and "nomisma.org/id/" in u and u not in labels:
                label(u, (el.text or "").strip(), labels)
        desc = root.find(f".//{H}findspot/{H}description")
        date = lambda k: (d.text if (d := root.find(f".//{H}{k}/{H}date")) is not None else None)
        h = {"id": hid, "igch": int(hid[4:]), "uri": f"http://coinhoards.org/id/{hid}",
             "findspot": (desc.text or "").strip() if desc is not None else None,
             "deposit": date("deposit"), "discovery": date("discovery"), "works": works}
        for k, (tag, role) in {**KINDS, **APART}.items():
            h[k], unsure = members(root, tag, role, labels)
            if unsure:
                h.setdefault("uncertain", {})[k] = unsure
        hoards.append(h)
    lp.write_text(json.dumps(dict(sorted(labels.items())), indent=1, ensure_ascii=False) + "\n")
    (HERE / "truth/hoards.json").write_text(json.dumps({"source": "CoinHoards IGCH NUDS, ODbL; labels nomisma.org, CC BY",
                                                        "hoards": hoards}, indent=1, ensure_ascii=False) + "\n")
    differ = sum(v["nuds_inline"] and v["label"] != v["nuds_inline"] for v in labels.values())
    print(f"hoards {len(hoards)}  nomisma ids {len(labels)}  label differs from NUDS inline: {differ}  "
          f"no nomisma label: {sum(v['source'] != 'nomisma' for v in labels.values())}")
    for k in KINDS:
        sizes = sorted(len(h[k]) for h in hoards)
        print(f"  {k:14} sizes {sizes}")


if __name__ == "__main__":
    main()
