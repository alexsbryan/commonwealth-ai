#!/usr/bin/env python3
"""ANS TEI monographs -> stripped markdown, one file per work, plus the hoard references
harvested BEFORE stripping. Reads raw/tei-ebooks (git clone of
github.com/AmericanNumismaticSociety/tei-ebooks, CC BY-NC 4.0) and raw/coinhoards.

RULE W (which works), fixed before any yield was seen: the TEI header carries the keyword
nomisma `greek_numismatics` AND (the author's surname is Newell, Thompson or Troxell — the
three the pre-reg names — OR the title contains "Alexander").

RULE H (which hoards a work "discusses"), fixed likewise:
  H1  the work's TEI holds <ref target="http://coinhoards.org/id/igchNNNN">, or
  H2  the IGCH record's own bibliography cites the work as "NNM <issue>" (the only ANS
      series IGCH cites by number; checked: 69 such citations, no "NS <n>" form).
Non-IGCH coinhoards ids (ch.*, change.*) are counted and not used.

STRIP: the whole teiHeader (it indexes every nomisma person and place), every attribute
(so every ref target / corresp / xml:id), graphics, figures and page breaks; any URL left
in printed text. Printed citations such as "IGCH 1664" or a Price number stay: they are the
book's words and name a hoard, not its contents. The run REFUSES if a truth host survives.

    python3 build_text.py      # writes text/*.md, text/ans.md, manifest.json, truth/hoard_refs.json
"""
import json, pathlib, re, sys
import xml.etree.ElementTree as ET

HERE = pathlib.Path(__file__).resolve().parent
TEI, NUDS = HERE / "raw/tei-ebooks/tei", HERE / "raw/coinhoards/nuds"
SOURCE = "https://github.com/AmericanNumismaticSociety/tei-ebooks/blob/master/tei/{}"
LICENCE = "CC BY-NC 4.0 (local use only; not redistributed, mesh_sharing=false)"
AUTHORS = {"Newell", "Thompson", "Troxell"}
T = "{http://www.tei-c.org/ns/1.0}"
SKIP = {"teiHeader", "graphic", "figure", "pb", "titlePage"}
LEAF = {"p", "item", "note", "byline", "closer", "bibl", "q", "titlePart", "signed", "dateline"}
NESTS = {"p", "list", "item", "table", "note", "q", "listBibl", "bibl", "byline", "closer", "front", "body", "back", "text"}
URL = re.compile(r"(https?://|www\.)\S+")
LEAK = re.compile(r"nomisma\.org|nomisma_|coinhoards|numismatics\.org|geonames\.org|pleiades\.stoa|viaf\.org|wikidata|xml:id|corresp=|target=", re.I)


def inner(el):
    return (el.text or "") + "".join(flat(c) for c in el)


def flat(el):
    """Inline text of a child, tail included: tags, attributes and skipped subtrees gone."""
    tag = el.tag.replace(T, "")
    marker = tag == "ref" and (el.get("target") or "").startswith("#") and "".join(el.itertext()).strip().isdigit()
    if tag in SKIP or tag == "note" or marker:   # notes are emitted as their own block by walk()
        return el.tail or ""
    return inner(el) + " " * (tag in ("lb", "cell")) + (el.tail or "")


def clean(s):
    return re.sub(r"\s+", " ", URL.sub("", s)).strip()


def walk(el, depth, out):
    tag = el.tag.replace(T, "")
    if tag in SKIP:
        return
    if tag == "head":
        out.append("#" * min(depth + 1, 6) + " " + clean(inner(el)))
    elif tag == "table":
        out.append("\n".join("| " + " | ".join(clean(inner(c)) for c in r.iter(T + "cell")) + " |"
                             for r in el.iter(T + "row")))
    elif tag in LEAF and not any(c.tag.replace(T, "") in NESTS for c in el.iter() if c is not el):
        out.append(("- " if tag == "item" else "Note: " if tag == "note" else "") + clean(inner(el)))
    else:                                        # a container: inline runs between block children
        run = el.text or ""
        for c in el:
            if c.tag.replace(T, "") in NESTS | {"head"} or c.tag.replace(T, "").startswith("div"):
                out.append(clean(run)); run = c.tail or ""
                walk(c, depth + tag.startswith("div"), out)
            else:
                run += flat(c)
        out.append(clean(run))
        return
    for n in el.iter(T + "note"):                # notes inside a leaf block follow it
        if n is not el:
            walk(n, depth, out)


def header(root):
    h = root.find(T + "teiHeader")
    txt = lambda p: clean("".join(h.find(p).itertext())) if h.find(p) is not None else ""
    return {"title": txt(f".//{T}titleStmt/{T}title"), "author": txt(f".//{T}titleStmt/{T}author/{T}name"),
            "date": txt(f".//{T}publicationStmt/{T}date"), "series": txt(f".//{T}seriesStmt/{T}title"),
            "issue": txt(f".//{T}seriesStmt/{T}biblScope"),
            "greek": any("greek_numismatics" in (t.get("ref") or "") for t in h.iter(T + "term"))}


def main():
    (HERE / "text").mkdir(exist_ok=True); (HERE / "truth").mkdir(exist_ok=True)
    manifest, refs, other = [], {}, 0
    for f in sorted(TEI.glob("*.xml")):
        raw = f.read_text(encoding="utf8")
        root = ET.fromstring(raw)
        if root.find(T + "teiHeader") is None:    # nine stub files are not TEI-namespaced books
            continue
        h = header(root)
        if not (h["greek"] and (h["author"].split(",")[0] in AUTHORS or "alexander" in h["title"].lower())):
            continue
        out = [f"# {h['title']} ({h['author'].split(',')[0]}, {h['date']})"]
        walk(root.find(T + "text"), 1, out)
        md = "\n\n".join(b for b in out if b.strip("#- |")) + "\n"
        if (m := LEAK.search(md)):
            sys.exit(f"REFUSED: {f.name} still carries {m.group(0)!r} after stripping")
        (HERE / "text" / f"{f.stem}.md").write_text(md, encoding="utf8")
        ids = set(re.findall(r"coinhoards\.org/id/(igch\d{4})", raw))
        other += len(set(re.findall(r"coinhoards\.org/id/((?:ch|change)\.[\w.]+)", raw)))
        for i in ids:
            refs.setdefault(i, {}).setdefault(f.stem, []).append("H1")
        manifest.append({"work": f.stem, **{k: h[k] for k in ("title", "author", "date", "series", "issue")},
                         "source_url": SOURCE.format(f.name), "licence": LICENCE,
                         "words": len(md.split()), "hoards_H1": len(ids)})
    nnm = {m["issue"]: m["work"] for m in manifest if m["series"] == "Numismatic Notes and Monographs"}
    for n in sorted(NUDS.glob("igch*.xml")):
        cited = re.findall(r"<reference[^>]*>(.*?)</reference>", n.read_text(encoding="utf8"), re.S)
        for issue in {i for c in cited for i in re.findall(r"\bNNM\s*(\d+)\b", c)} & nnm.keys():
            refs.setdefault(n.stem, {}).setdefault(nnm[issue], []).append("H2")
    for m in manifest:
        m["hoards_H2"] = sum("H2" in v.get(m["work"], []) for v in refs.values())
    (HERE / "text" / "ans.md").write_text("\n".join((HERE / "text" / f"{m['work']}.md").read_text(encoding="utf8")
                                                    for m in manifest), encoding="utf8")
    (HERE / "manifest.json").write_text(json.dumps({"rule_W": __doc__.split("RULE H")[0].split("RULE W")[1].strip(),
        "licence": LICENCE, "works": manifest, "total_words": sum(m["words"] for m in manifest),
        "non_igch_coinhoards_refs_ignored": other}, indent=1, ensure_ascii=False) + "\n")
    (HERE / "truth" / "hoard_refs.json").write_text(json.dumps(dict(sorted(refs.items())), indent=1) + "\n")
    print(f"works {len(manifest)}  words {sum(m['words'] for m in manifest)}  hoards {len(refs)} "
          f"(H1 {sum(any('H1' in r for r in v.values()) for v in refs.values())}, "
          f"H2 {sum(any('H2' in r for r in v.values()) for v in refs.values())})")
    for m in manifest:
        print(f"  {m['work']:12} {m['author'].split(',')[0]:9} {m['date']} {m['series'][:3]} {m['issue']:>3} "
              f"{m['words']:>7}w H1={m['hoards_H1']:<3} H2={m['hoards_H2']:<3} {m['title'][:50]}")


if __name__ == "__main__":
    main()
