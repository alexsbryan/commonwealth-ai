# ralph — the the-link campaign, one unit per session

Same protocol as `ralph/next/ring-guest-substrate/PROMPT.md`, of which this
file is an instance. Read that file first; everything in it applies, with
these substitutions and none other:

- Campaign: `the-link` (`quality/campaigns/the-link.toml`, child of
  ring-apps). Bars are `tl-*`.
- Order: `.sovereign/features/the-link/order.md` — its Demo section is the
  definition of done, its Premises are what the inventory row verifies, its
  Decisions (D1–D4) are closed and not yours.
- Queue: THIS file's sibling, `ralph/next/the-link/STATE.md`. Pointer keys
  are defined at its top. `ralph/next/ring-guest-substrate/STATE.md` is
  ANOTHER campaign's queue — never open it, never mark it.
- Regression sets, never edited from this queue: `quality/campaigns/ring-room.toml`
  (six rr-2 bars) and `quality/campaigns/ring-guest.toml` (five rg bars).
- One absolute this campaign adds to the shared protocol:
  `commonwealth/crates/commonwealth-rail-core` is READ-ONLY (charter,
  invariant `c3fed9c3`). If a row seems to need an edit there, you have found
  the order's exit condition — stop (§6) and say so; never improvise around it.

Row prefixes resolve as in the shared protocol: `tl-` builds (§3),
`REVIEW-build-the-link-` builds with judgment, `REVIEW-DEMO-the-link-` runs
the demo in the MAIN workdir, `REVIEW-audit-the-link-` is the full gate and
principles review (§4), `HUMAN-the-link-` is never done by you — write
`ralph/NEEDS_HUMAN.md` and stop (§6).
