# journey-corpus — the CLI journey harness's built-in fixture corpus

These three documents exist so `sovereign/docs/cli-contract.toml`'s corpus
journeys can run end to end on any machine, with no network and no model
download.

## Why a fixture corpus exists at all

Before this, `{corpus}` had no default value. The installable catalog is six
real corpora and the smallest is a ~0.5 GB download, which is not something a
routine local guard should do behind your back — so the token stayed unset and
every step that referenced it reported `skip … no fixture`.

That was honest, and it was also most of the coverage gap. `{corpus}` is the
single most demanded fixture token in the manifest — 25 of its placeholder
occurrences — and its absence left four journeys executing *nothing at all*
(`mcp-interop`, `enrich-atlas`, `recipe-author`, `spec-check`) while
`corpus-lifecycle` ran one of its six steps. The behavioural coverage of the
whole manifest sat at 23%.

## Why it is installed through a recipe, not `corpus ingest`

`corpus ingest <folder>` would build an index from this directory in one
command, and it would be the wrong thing to test. `corpus-lifecycle` asserts
`corpus install`, which is a different code path: the daemon resolves a
recipe, validates its declared parameters, acquires, extracts, chunks, and
indexes. Substituting the easier command would make the lane green while
leaving the path a user actually takes unverified — the exact trade the
journey layer exists to refuse.

So the fixture ships a real recipe (`journey-corpus.recipe.toml`) whose
`acquire.type` is `local_file` pointing at this directory. `corpus install`
runs its genuine machinery; only the bytes are local.

## Why the content is what it is

The documents are small enough to index in seconds and deliberately carry
distinctive, unambiguous terms — `quicksilver-ledger`, `tessellate-harbour`,
`nine-banded-armadillo` — so a retrieval assertion can demand a specific
result rather than "something came back". A fixture whose vocabulary overlaps
the rest of the repo cannot distinguish a working search from a lucky one.

Keep them small. This corpus is a test fixture, not a demo.
