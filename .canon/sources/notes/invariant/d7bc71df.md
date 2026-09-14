# ONE MALFORMED FILE UNDER quality/campaigns/ BREAKS EVERY CAMPAIGN'S READ, NOT JUST ITS OWN. scripts/co-lineage.py loads the whole directory…

ONE MALFORMED FILE UNDER `quality/campaigns/` BREAKS EVERY CAMPAIGN'S READ, NOT JUST ITS OWN. `scripts/co-lineage.py` loads the whole directory and raises on the FIRST contract violation, so `coverage <any-campaign>`, `list`, and every other campaign read exit 3 until the offender is fixed. Blast radius is the directory, not the file.

OBSERVED 2026-09-04: I authored `quality/campaigns/cw-lift.toml` (campaign cw-lift, 7 bars) and never ran the loader on it. It violated the schema four ways and took down every other campaign's read with it. A peer on another machine diagnosed and fixed it in `1bcd7b168` while I was mid-campaign on a 21-commit-stale tree. The loader was present locally the whole time (`scripts/co-lineage.py`, dated Aug 26) — this was not a missing instrument, it was an unrun one.

THE CONTRACT, as `1bcd7b168` establishes it:
- campaign `status` closed set: `active` | `closed`. NOT `open`.
- bar `status` closed set: `open` | `deferred` | `descoped`. Prose values
  (`at-risk`, `re-derived`, `answered — ...`) are violations; keep the prose in
  a COMMENT beside the bar, where it reads the same and parses.
- every `floor` requires a sibling `floor_basis` citing the measurement it came
  from — date, commit, instrument.
- `direction` closed set does NOT include `tracked`. To track a number without
  gating it, use `lower_is_better` with the expected ceiling as `floor` and say
  so in a comment. Do not invent a number to satisfy the schema.

THE RULE: a data file authored FOR an instrument is not landed until that
instrument has read it. `python3 scripts/co-lineage.py list` is ~1s and is the
check. This is ARCH §18.1 ("a gate you have not watched fail is not a gate")
applied to the authoring side: I wrote the input to a gate and never watched
the gate accept it.

The strictness is deliberate and is NOT to be softened — operator, 2026-09-04:
invariants at this level; a skip-with-notice is a one-way ticket to
degradation. Do not "fix" a future occurrence by making the loader skip bad
files.
