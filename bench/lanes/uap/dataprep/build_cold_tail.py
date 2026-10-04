#!/usr/bin/env python3
"""Transform the 10,750-case NARA metadata table into a lean, ingest-ready
JSONL for the cold-tail "metadata search" corpus (uap-blue-book-index).

Cold-tail records carry NO OCR'd narrative — just the structured metadata
(location + date + NARA handle) synthesized into a short searchable
`content` line. They embed + index cheaply (1 chunk each → ConvBucket::Tiny
→ no RAPTOR/LLM) and get NO investigation enrichment; the deep typed graph
lives only on the hero (unidentified) set. The bulky image_urls arrays are
dropped (not needed for search; the hero pipeline already consumed them).
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def main() -> int:
    src = os.path.join(HERE, "metadata.jsonl")
    out = os.path.join(HERE, "cold_tail.jsonl")
    n = 0
    with open(src) as f, open(out, "w") as w:
        for line in f:
            if not line.strip():
                continue
            r = json.loads(line)
            loc = r.get("location") or "(location unknown)"
            date = (r.get("date") or "")[:10]
            unid = r.get("is_unidentified")
            disp = "officially UNIDENTIFIED" if unid else "see case file for disposition"
            content = (
                f"U.S. Air Force Project Blue Book UFO case file. "
                f"Location: {loc}. Reported: {date}. "
                f"Status: {disp}. "
                f"({r.get('n_images', 0)} scanned pages; NARA fileUnit {r.get('naId')})."
            )
            w.write(
                json.dumps(
                    {
                        "case_id": str(r.get("nicap_case_no") or r.get("naId")),
                        "naId": r.get("naId"),
                        "title": f"{loc} ({date})",
                        "content": content,
                        "location": loc,
                        "date": date,
                        "is_unidentified": bool(unid),
                        "source": "nara_aws_odr_rg341",
                    }
                )
                + "\n"
            )
            n += 1
    print(f"cold-tail records: {n} → {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
