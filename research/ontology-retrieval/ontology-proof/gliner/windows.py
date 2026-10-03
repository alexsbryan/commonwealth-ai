#!/usr/bin/env python3
"""G-T0 part 3: chunk length against the model window, on ft-ans-dev-b's cached chunks.

    python3 windows.py [--json windows.json]

Model-free. For every chunk: whitespace words, regex tokens (the splitter both gline-rs and the
`gliner` package use), and what each version of the seam hands the model:

  pre-fix   (before f4a695e9a): the whole chunk; gline-rs keeps the first 512 regex tokens and
            drops the rest without a word (splitter.rs:42-45).
  windowed  (HEAD, gliner_ner.rs:337-360): 300-word windows, 30 shared; gline-rs still caps
            EACH window at 512 regex tokens, so a window whose 300 words carry > 512 tokens
            loses its tail too. A tail is truly lost only if the next window does not start
            before the cut.

Also: windows over the model's trained length (gliner_config max_len 384), which the Python
reference would truncate and gline-rs runs anyway.
"""
import argparse, json, pathlib, sys

import lance

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from seam import GLINE_MAX_LENGTH, SPLIT_RX, word_windows  # noqa: E402

IDX = pathlib.Path.home() / ".svrnmesh/indexes/ft-ans-dev-b"
MAX_LEN = 384  # urchade/gliner_small-v2.1 gliner_config.json "max_len"


def cut_at(text, limit):
    """Char offset where gline-rs stops reading `text` (None if it reads it all)."""
    toks = list(SPLIT_RX.finditer(text))
    return toks[limit - 1].end() if len(toks) > limit else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--json", default=str(HERE / "windows.json"))
    a = ap.parse_args()
    t = lance.dataset(str(IDX / "chunks.lance")).to_table(columns=["id", "content"]).to_pydict()
    rows, totals = [], {"chunks": 0, "chars": 0, "prefix_dropped_chars": 0, "windowed_dropped_chars": 0}
    for cid, text in sorted(zip(t["id"], t["content"])):
        totals["chunks"] += 1
        totals["chars"] += len(text)
        pre = cut_at(text, GLINE_MAX_LENGTH)
        pre_lost = len(text.rstrip()) - pre if pre else 0
        wins = word_windows(text)
        seen = [False] * len(text)
        wrow = []
        for shift, w in wins:
            c = cut_at(w, GLINE_MAX_LENGTH)
            end = shift + (c if c else len(w))
            for i in range(shift, end):
                seen[i] = True
            wrow.append({"start": shift, "words": len(w.split()), "regex_tokens": len(SPLIT_RX.findall(w)),
                         "cut_at": None if c is None else shift + c})
        lost = sum(1 for i, ch in enumerate(text) if not seen[i] and not ch.isspace())
        totals["prefix_dropped_chars"] += pre_lost
        totals["windowed_dropped_chars"] += lost
        rows.append({"chunk": cid, "chars": len(text), "words": len(text.split()),
                     "regex_tokens": len(SPLIT_RX.findall(text)), "prefix_cut_at": pre,
                     "prefix_lost_chars": pre_lost, "windows": wrow, "windowed_lost_nonspace_chars": lost,
                     "windows_over_max_len": sum(w["regex_tokens"] > MAX_LEN for w in wrow)})
    over = [r for r in rows if r["prefix_cut_at"] is not None]
    summary = {
        **totals,
        "chunks_cut_prefix": [{"chunk": r["chunk"], "regex_tokens": r["regex_tokens"], "lost_chars": r["prefix_lost_chars"]} for r in over],
        "chunks_split_windowed": [r["chunk"] for r in rows if len(r["windows"]) > 1],
        "windows_cut_by_gline": [{"chunk": r["chunk"], **w} for r in rows for w in r["windows"] if w["cut_at"] is not None],
        "chunks_losing_text_windowed": [{"chunk": r["chunk"], "lost_nonspace_chars": r["windowed_lost_nonspace_chars"]}
                                        for r in rows if r["windowed_lost_nonspace_chars"]],
        "windows_over_max_len_384": sum(r["windows_over_max_len"] for r in rows),
        "windows_total": sum(len(r["windows"]) for r in rows),
    }
    json.dump({"summary": summary, "chunks": rows}, open(a.json, "w"), indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
