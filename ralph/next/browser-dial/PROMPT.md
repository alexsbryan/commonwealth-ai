# ralph — the browser-dial campaign, one unit per session

Same protocol as `ralph/next/ring-guest-substrate/PROMPT.md`, of which this
file is an instance. Read that file first; everything in it applies, with
these substitutions and none other:

- Campaign: `browser-dial` (`quality/campaigns/browser-dial.toml`, child of
  the-link). Bars are `bd-*`.
- Order: `ralph/next/browser-dial/order.md` (the committed copy of
  `.sovereign/features/browser-dial/order.md`, which may hold later local
  corrections) — its Demo section is the definition of done, its Premises
  are what the inventory row verifies, its Predictions are what the audit
  reads the diff against.
- Queue: THIS file's sibling, `ralph/next/browser-dial/STATE.md`.
- Regression sets, never edited from this queue:
  `quality/campaigns/ring-room.toml`, `ring-guest.toml`, `the-link.toml`.
- One absolute this campaign adds to the shared protocol: the product diff
  is EMPTY — `git diff` over sovereign/crates and commonwealth/crates for
  every row is zero, and a row that seems to need an edit there has found
  the order's exit condition — stop (§6) and say so; never improvise
  around it. Probes and the page live under `target/`, never committed.

Row prefixes resolve as in the shared protocol: `bd-` builds (§3) — here
"builds" means MEASURES (the honest negative passes, the-link D3
inherited); `REVIEW-build-browser-dial-` builds with judgment;
`REVIEW-audit-browser-dial-` is the full gate and principles review (§4).
