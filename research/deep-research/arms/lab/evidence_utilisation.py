#!/usr/bin/env python3
"""How much of the acquired window actually reaches the deliverable.

THE POINT: a judged cell costs ~45.7 minutes and the per-draw sd is 2.97, so a
volume/curation curve measured by SCORE alone is unaffordable (a 4-point curve
at n=3 is ~9 hours). Every number here is DETERMINISTIC and free — it reads a
finished run tree, makes no judge call, and takes milliseconds. Use it to shape
the curve, then spend judge time only on the finalists.

WHAT IT IS NOT: a quality measure. Citing more of the window is not the same as
writing a better report, and this script must never be reported as if it were
(§18.4 — validate the instrument before the result). It measures REACH:
how much of what we acquired the writer was shown, and how much it used.

    evidence_utilisation.py <run-dir> [<run-dir> ...]
    evidence_utilisation.py --glob 'runs-*/*/dr-*'
"""
import json, re, sys, glob as globmod
from pathlib import Path

PASSAGE_CHARS = 1400          # synthesize.rs PASSAGE_CHARS


def read_window(run: Path):
    """Window size, summed ACROSS ROUNDS — never unioned by id.

    HISTORY, AND WHY THE SUM SURVIVED THE FIX. `ev-N` used to be PER-ROUND
    POSITIONAL: round 1 emitted ev-1..ev-49 and round 2 RESTARTED at ev-1 for
    a different set of chunks. Unioning by id therefore COLLAPSED round 2 onto
    round 1 — it under-counted the window and made legitimate high-numbered
    citations look out of range, which produced a false "the writer fabricates
    citation handles" finding on 2026-08-26, retracted the same day.

    Since 2026-08-27 the counter is RUN-scoped (fetch.rs mints from the
    controller's `next_evidence_id`), so ids are globally unique and unioning
    would now give the same answer. The sum is kept anyway: it is correct
    under BOTH numberings, so this script reads an old run tree and a new one
    without a version check. Corroboration: it matches `source-registry.json`
    (57 vs 57, 60 vs 60).

    STILL A LOWER BOUND, and this part the fix did not change. compose_report
    witnessed `window_chunks=61` on a run whose dumped rounds sum to 57, so
    the merged window it actually writes from is larger than anything
    persisted. Read the trace's `window_chunks` when you have the cell log;
    this is the best the run dir alone supports.
    """
    total_chunks, chars, srcs, refused = 0, 0, set(), 0
    for f in sorted(run.glob("evidence-window-*.json")):
        try:
            w = json.loads(f.read_text())
        except Exception:                                   # noqa: BLE001
            continue
        refused = max(refused, len(w.get("dedup_refused") or []))
        for c in w.get("chunks") or []:
            total_chunks += 1
            chars += len(c.get("content") or "")
            srcs.add(c.get("source_url") or "?")
    return total_chunks, chars, srcs, refused


def report_text(run: Path) -> str:
    for name in ("report.md", "render-race.md"):
        p = run / name
        if p.is_file():
            return p.read_text()
    return ""


def measure(run: Path) -> dict | None:
    chunks, chars, srcs, refused = read_window(run)
    rep = report_text(run)
    if not chunks or not rep:
        return None
    cited = {int(h.split("-")[1]) for h in re.findall(r"ev-\d+", rep)}
    # NO FABRICATION METRIC. Deciding whether a handle is real needs the
    # MERGED compose-time window, which is never persisted (the run dir has
    # only per-round dumps, and the merged window is larger than their sum).
    # The earlier `dangling` count compared a handle NUMBER against a window
    # COUNT and was wrong twice over. `max_cited` is reported as an
    # observation; a value above `chunks` means the merged window is bigger
    # than the dumps, NOT that the writer invented anything.
    #
    # `cited` UNDER-COUNTS ON A PRE-2026-08-27 RUN TREE and is exact after.
    # Before the run-scoped counter, one handle could name several chunks, so
    # a report citing ev-2 was reaching an unknown number of them; the
    # denominator was honest and the numerator was not. Utilisation measured
    # across that boundary is not comparable — re-mint, do not mix.
    return {
        "run": run.name,
        "chunks": chunks,
        "window_chars": chars,
        "sources": len(srcs),
        "dedup_refused": refused,
        "words": len(rep.split()),
        "cited": len(cited),
        "max_cited": max(cited) if cited else 0,
        "util_pct": 100.0 * len(cited) / chunks,
        "shown_pct_8": 100.0 * 8 * PASSAGE_CHARS / chars,
        "shown_pct_28": 100.0 * 28 * PASSAGE_CHARS / chars,
    }


def main(argv):
    runs = []
    oneline = False
    if argv and argv[0] == "--oneline":
        oneline, argv = True, argv[1:]
    if argv and argv[0] == "--glob":
        runs = [Path(p) for p in sorted(globmod.glob(argv[1]))]
    else:
        runs = [Path(a) for a in argv]
    rows = [r for r in (measure(p) for p in runs if p.is_dir()) if r]
    if not rows:
        sys.exit("no run dir carried both an evidence window and a report")
    if oneline:
        # LABELLED, because a harness prints this with `tail -1` and an
        # unlabelled row of six integers is not glassbox (§9.1).
        for r in rows:
            print(f"cited {r['cited']}/{r['chunks']} chunks "
                  f"({r['util_pct']:.0f}%)  max_handle=ev-{r['max_cited']}  "
                  f"words={r['words']:,}  window={r['window_chars']:,}c "
                  f"(chunk count is a LOWER BOUND — see read_window)")
        return rows
    hdr = (f"{'run':<16}{'chunks':>7}{'win chars':>12}{'words':>8}"
           f"{'cited':>7}{'maxev':>7}{'util%':>7}{'shown/sec @8':>14}")
    print(hdr); print("-" * len(hdr))
    for r in rows:
        print(f"{r['run']:<16}{r['chunks']:>7}{r['window_chars']:>12,}"
              f"{r['words']:>8,}{r['cited']:>7}{r['max_cited']:>7}"
              f"{r['util_pct']:>6.0f}%{r['shown_pct_8']:>13.2f}%")
    return rows


if __name__ == "__main__":
    main(sys.argv[1:])
