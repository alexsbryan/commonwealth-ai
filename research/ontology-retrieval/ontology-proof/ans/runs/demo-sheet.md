# Demo sheet — ei7 ontology proof (rule-picked, from the committed board)

Board: `runs/scoreboard.json` (identity: synth Qwen3.6-35B-A3B-MTP-UD-Q6_K, judge Qwen3.5-4B-UD-Q6_K_XL). Ratified bars: none claimed — see below.

## Claim posture, by the pre-reg's claim rule

A kind is claimed only if bars 1-5 pass for it. Measured here:

| kind | delta (full - max(deep, ablation)) | bar 2 | why not claimed |
|---|---|---|---|
| k0_look_it_up | -0.286 | not passed | ablation arm never-ran (bar 2's second delta unmeasured); delta -0.286 below max(0.15, 2x0.05) |
| k1_list_them_all | -0.138 | not passed | ablation arm never-ran (bar 2's second delta unmeasured); delta -0.138 below max(0.15, 2x0.05) |

**No kind is claimed. The demo shows the K0 tie and the per-kind attribution.**

## The K0 tie (the no-gain-predicted control)

| arm | K0 judge |
|---|---|
| bare | 0.857 |
| deep | 0.857 |
| full | 0.571 |

The prediction was a tie; the measurement is full losing by 0.29 — bar 1 goes the
wrong way in the shipped default itself (diagnostic arms, PRE-REG Deviations).
This is on stage because the control failing is stronger evidence than any single
win would have been.

## The receipts (closest margin per kind, from the side-by-side)

The three-way side-by-side carries 47 questions with members marked found / missed / fabricated; see `runs/sidebyside.md`. The showcase picker (median
gain per claimed kind) has nothing to pick: the claim rule is the gate.

## The deck (pick a card)

Empty, by rule: a deck exists only for claimed kinds, and none is claimed. A live
draw still runs — `demo/live-run.sh "<question>"` — with the measured odds stated
before the draw and any disagreement said out loud.
