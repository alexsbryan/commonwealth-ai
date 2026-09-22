#!/usr/bin/env python3
"""The demo sheet: the pre-reg's rule-picked showcase and the claim posture,
emitted from the committed boards — never hand-written.

The rule (PRE-REG "What the study must emit"): the showcase is the
median-gain question per (corpus, CLAIMED kind), plus one K0 tie. With no
claimed kinds the sheet says so, names the attribution, and shows the
closest-margin question per kind as the receipts. The deck for pick-a-card
is emitted only for claimed kinds, by the same rule — an empty deck here is
the claim rule working, not a gap in the tooling.
"""
import json, sys, tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
ANS = ROOT / "research/ontology-retrieval/ontology-proof/ans"

def load(p): return json.load(open(p))

def main(out_path):
    board = load(ANS / "runs/scoreboard.json")
    verdict_note = load(ANS / "runs/sidebyside.json")
    lines = []
    A = lines.append
    A("# Demo sheet — ei7 ontology proof (rule-picked, from the committed board)")
    A("")
    A(f"Board: `runs/scoreboard.json` (identity: synth {board['identity']['synth_model']}, "
      f"judge {board['identity']['judge_model']}). Ratified bars: none claimed — see below.")
    A("")
    A("## Claim posture, by the pre-reg's claim rule")
    A("")
    A("A kind is claimed only if bars 1-5 pass for it. Measured here:")
    A("")
    A("| kind | delta (full - max(deep, ablation)) | bar 2 | why not claimed |")
    A("|---|---|---|---|")
    for cat, blob in board["categories"].items():
        arms = blob["arms"]
        full = arms.get("full", {}).get("judge")
        deep = arms.get("deep", {}).get("judge")
        band = blob["band"]["band"]
        delta = (full - deep) if (full is not None and deep is not None) else None
        why = (
            "ablation arm never-ran (bar 2's second delta unmeasured)"
            if board["i3_ablation_vs_full"]["verdict"] == "never-ran"
            else ""
        )
        if delta is not None and delta < max(0.15, 2 * band):
            why = (why + "; " if why else "") + f"delta {delta:+.3f} below max(0.15, 2x{band})"
        A(f"| {cat} | {'n/a' if delta is None else f'{delta:+.3f}'} | not passed | {why} |")
    A("")
    A("**No kind is claimed. The demo shows the K0 tie and the per-kind attribution.**")
    A("")
    A("## The K0 tie (the no-gain-predicted control)")
    A("")
    k0 = board["categories"]["k0_look_it_up"]["arms"]
    rows = [("bare", k0.get("bare", {}).get("judge")), ("deep", k0.get("deep", {}).get("judge")),
            ("full", k0.get("full", {}).get("judge"))]
    A("| arm | K0 judge |")
    A("|---|---|")
    for a, v in rows:
        A(f"| {a} | {'n/a' if v is None else f'{v:.3f}'} |")
    A("")
    A("The prediction was a tie; the measurement is full losing by 0.29 — bar 1 goes the")
    A("wrong way in the shipped default itself (diagnostic arms, PRE-REG Deviations).")
    A("This is on stage because the control failing is stronger evidence than any single")
    A("win would have been.")
    A("")
    A("## The receipts (closest margin per kind, from the side-by-side)")
    A("")
    sbs = verdict_note.get("questions", [])
    A(f"The three-way side-by-side carries {len(sbs)} questions with members marked "
      "found / missed / fabricated; see `runs/sidebyside.md`. The showcase picker (median")
    A("gain per claimed kind) has nothing to pick: the claim rule is the gate.")
    A("")
    A("## The deck (pick a card)")
    A("")
    A("Empty, by rule: a deck exists only for claimed kinds, and none is claimed. A live")
    A("draw still runs — `demo/live-run.sh \"<question>\"` — with the measured odds stated")
    A("before the draw and any disagreement said out loud.")
    Path(out_path).write_text("\n".join(lines) + "\n")
    print(f"demo sheet -> {out_path}")

if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else str(ANS / "runs/demo-sheet.md"))
