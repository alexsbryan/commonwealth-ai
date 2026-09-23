#!/usr/bin/env python3
"""Study-1 K0/K1 evidence comparison over exact quoted spans.

Do not use title as passage identity: multiple chunks share each section title.
This instrument checks the answer-bearing quote against the *admitted* text,
then compares passage overlap by content. It cannot identify the pipeline step
which discarded an absent passage; that needs pre/post-step provenance.
"""

import hashlib
from collections import Counter

from w1_determinism import load


QUOTES = {
    "lookup-agrinion-acquired": "summer of 1962",
    "lookup-demanhur-first-notice": "Mr. Khayat",
    "lookup-histiaea-oxidation": "action of sea water",
    "lookup-megara-obverse-dies": "Fifty-four obverse dies",
    "list-igch0076-mints": "Kyparissia",
}


def passage_key(chunk):
    body = chunk.get("prompt_text") or chunk["snippet"]
    return chunk["corpus_id"], chunk["title"], hashlib.sha256(body.encode()).hexdigest()


def report():
    for question, quote in QUOTES.items():
        bare = load("bare", 1)[question]
        bare_passages = {passage_key(c) for c in bare["retrieved"] if c.get("in_prompt")}
        print(f"{question}: bare judge={bare['synth']['judge_fact_score']['ratio']:.3f}")
        for arm in ("bare", "grounding-only", "full"):
            for run in (1, 2, 3):
                row = load(arm, run)[question]
                admitted = [c for c in row["retrieved"] if c.get("in_prompt")]
                hits = [i for i, c in enumerate(admitted) if quote.casefold() in c["prompt_text"].casefold()]
                overlap = len(bare_passages & {passage_key(c) for c in admitted})
                docs = Counter(c["title"].split("›")[0].strip() for c in admitted)
                print(
                    f"  {arm} run-{run}: quoted-in-prompt={hits}, "
                    f"bare-passage-overlap={overlap}/{len(bare_passages)}, "
                    f"admitted={len(admitted)}, documents={len(docs)}, "
                    f"judge={row['synth']['judge_fact_score']['ratio']:.3f}"
                )


if __name__ == "__main__":
    report()
