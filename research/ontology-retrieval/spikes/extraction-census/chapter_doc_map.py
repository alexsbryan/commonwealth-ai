#!/usr/bin/env python3
"""The chapter -> document map both censuses read, from one place.

`chapter_doc_map.json` (hand-built for the 2026-09-19 spike, gitignored) is used
when it sits next to the census output. Otherwise the map is DERIVED from the
index's own `chapters.json`, which is all a re-census on another corpus has.

Derivation. `build_corpus.py` writes one `# <Service> — <Doc>` heading per source
document and demotes that document's own headings under it, so a chapter whose
title is `<svc> — <doc>` IS a document heading, and its lowest chunk id is where
that document starts. Chunk ids run in file order, so those start ids partition
the chunk space; a chapter belongs to the document holding most of its chunk ids,
and `multi_doc` says its chunks crossed a boundary. A `<svc>` seen in only one
title is not read as a document heading, because a section title can carry an em
dash too (`10A. International Transfers — EEA Data Transfers`, re-census
sec_00014). The cost is that a service contributing exactly ONE document would be
missed, so both censuses print `services` rather than leave it implicit.

Checked against the hand-built spike map, 449 chapters: `doc` agrees on 448,
`multi_doc`, `words` and `title` on 449 each. The one disagreement is a chapter
whose chunks cross a document boundary, where the hand map took the first
document and this takes the majority one.
"""
import argparse
import bisect
import collections
import json
import os
import re

DOC_TITLE = re.compile(r"(?P<svc>[^—]+) — (?P<doc>[^—]+)")


def chapters_path(corpus, data_root=None):
    root = data_root or os.environ.get("SOVEREIGN_DATA_DIR") or os.path.expanduser("~/.svrnmesh")
    return os.path.join(root, "indexes", corpus, "chapters.json")


def derive(chapters):
    """(map, services) from an index's `chapters` array. See the module doc."""
    heads = []
    for c in chapters:
        m = DOC_TITLE.fullmatch((c.get("title") or "").strip())
        if m and c.get("chunk_ids"):
            heads.append((m.group("svc").strip(), c["title"].strip(), min(c["chunk_ids"])))
    per_service = collections.Counter(s for s, _, _ in heads)
    services = sorted(s for s, n in per_service.items() if n >= 2)
    bounds = sorted((start, title) for svc, title, start in heads if svc in services)
    starts = [s for s, _ in bounds]
    titles = [t for _, t in bounds]

    def doc_of_chunk(chunk_id):
        i = bisect.bisect_right(starts, chunk_id) - 1
        return titles[i] if i >= 0 else None

    out = {}
    for c in chapters:
        docs = collections.Counter(doc_of_chunk(x) for x in (c.get("chunk_ids") or []))
        out[c["id"]] = {
            "doc": docs.most_common(1)[0][0] if docs else None,
            "title": (c.get("title") or c.get("first_line") or "").strip(),
            "words": c.get("word_count"),
            "multi_doc": len({d for d in docs if d}) > 1,
        }
    return out, services


def load(corpus, out_dir, data_root=None):
    """(map, source, services). `source` names which of the two paths was taken."""
    handmade = os.path.join(out_dir, "chapter_doc_map.json")
    if os.path.exists(handmade):
        m = json.load(open(handmade))
        return m, handmade, sorted({(v.get("doc") or " — ").split(" — ")[0] for v in m.values()} - {""})
    p = chapters_path(corpus, data_root)
    chapters = json.load(open(p))["chapters"]
    m, services = derive(chapters)
    return m, f"derived from {p}", services


def self_test():
    chapters = [
        # two document headings, one service, chunk ids in file order
        {"id": "sec_1", "title": "Spotify — Privacy Policy", "chunk_ids": [1], "word_count": 9},
        {"id": "sec_2", "title": "Spotify — Terms", "chunk_ids": [5], "word_count": 9},
        # a section wholly inside the first document
        {"id": "sec_3", "title": "Data we collect", "chunk_ids": [2, 3], "word_count": 80},
        # a section whose chunks straddle the boundary
        {"id": "sec_4", "title": "Contact us", "chunk_ids": [4, 5, 6], "word_count": 20},
        # a section title that carries an em dash and is NOT a document heading
        {"id": "sec_5", "title": "10A. Transfers — EEA", "chunk_ids": [7], "word_count": 30},
    ]
    m, services = derive(chapters)
    assert services == ["Spotify"], services
    assert m["sec_3"]["doc"] == "Spotify — Privacy Policy", m["sec_3"]
    assert m["sec_3"]["multi_doc"] is False, m["sec_3"]
    assert m["sec_4"]["doc"] == "Spotify — Terms", m["sec_4"]     # majority, 2 of 3
    assert m["sec_4"]["multi_doc"] is True, m["sec_4"]
    assert m["sec_5"]["doc"] == "Spotify — Terms", m["sec_5"]     # not its own document
    assert m["sec_3"]["words"] == 80, m["sec_3"]

    # planted failing input: drop the document headings and no chapter has a doc
    plain = [dict(c, title="Data we collect") for c in chapters]
    m2, services2 = derive(plain)
    assert services2 == [], services2
    assert all(v["doc"] is None for v in m2.values()), m2
    try:
        derive([{"id": "sec_1"}])          # no title, no chunk_ids
    except Exception as e:                  # noqa: BLE001 - the plant is that it must not raise
        raise AssertionError(f"derive must tolerate a bare chapter row, raised {e!r}")
    print("self-test: passed")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--corpus", default="spike-fineprint-census")
    ap.add_argument("--out", default=os.path.dirname(os.path.abspath(__file__)))
    a = ap.parse_args()
    if a.self_test:
        self_test()
    else:
        m, source, services = load(a.corpus, a.out)
        print(f"{len(m)} chapters, services {services}, source {source}")
