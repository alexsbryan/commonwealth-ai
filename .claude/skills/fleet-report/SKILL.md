---
name: fleet-report
description: Generate the weekly fleet context-spend + split-adoption report (scripts/fleet-report.py) and summarize what changed since the last one.
---

Run the weekly fleet report and interpret it. This is the memory-model initiative's
tracking instrument — it measures whether the fleet's behavior is actually shifting
(splits happening, ramps passing, preamble share falling), not just whether the
mechanisms exist.

## Steps

1. Run it from the project root (no arguments needed for the standard weekly run):

   ```
   python3 scripts/fleet-report.py
   ```

   Options: `--days N` (window, default 7), `--project <dir>` (default cwd),
   `--out-dir` (default `~/.sovereign/reports`). It writes `fleet-<date>.md`
   plus a `fleet-<date>.json` sidecar; the previous sidecar drives the trend
   column automatically.

2. Read the printed report and give the user a short verdict focused on **deltas
   and adoption**, not a restatement of the table:
   - Split protocol: red crossings vs splits honored; any session that lingered
     red for a long time is the headline failure.
   - Ramp gate: which frame-booted successors passed (≤5k raw, 0 repeats); cold
     sessions FAILing is expected noise, successors FAILing is a protocol break.
   - H2a preamble average vs previous report — the regime-change tracker.
     MEMORY_MODEL predicts it falls as frames replace re-acquisition.
   - Lever sizes trend (H1 splitting should shrink as splits actually happen).
   - Frame provenance at session end: self-reported = encode-time writes working;
     distilled-only = agents not banking frames (rescue path only).

3. If a metric moved sharply or a protocol break shows up, record it with
   `note` (kind `decision` or `todo`) so the next session inherits the finding.

## Caveats baked into the numbers

- Levers are independent counterfactuals and overlap — never sum them.
- Ramp is an upper bound for cold sessions (includes genuine pre-implementation
  research); the gate is only meaningful for frame-booted successors.
- H3 batching is an explicit upper bound.
- Commit counts come from git-commit tool calls in transcripts (H5 cadence proxy).
