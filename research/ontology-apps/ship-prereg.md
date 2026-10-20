# Ship bars for the ontology layer (pre-registered 2026-10-20, before any read or authoring below)

**The claim.** Raw input plus a declared ontology plus a patterned method yields typed, cited records. Most SaaS
applications are such records (a CRM's deals, a tracker's cases, a register of rules in force). The next use case
is expressible in the same language and runs through the same default commands.

**Engine and reads.** DeepSeek Flash through the daemon at HEAD, every question asked fresh (`ASKER=daemon
RESOLVE_ASKER=daemon`). One read per corpus (operator 2026-10-20: a scrappy pass, not two reads per arm), so each
number is reported beside its known single-read noise (Ward about three deals, REVIEW_BRIEF.md). Approved spend:
about 1M answer tokens plus twelve authoring sessions.

## The bars

**S1, deals in mail.** The five worlds and ward-story, one fresh read each (arm `ship-h1`). Passes if the worlds
serve at least 68/93 and attach at least 237/328 (the one-shot under c138d09b1), and ward-story serves at least
6/14 (the one-shot's 8, minus 2).

**S2, facts in a novel.** saltgrass-ledger, one fresh read: at least 23.5/42 (the one-shot mean of 25.5, minus 2).

**S3, a tracker's cases.** uv-support's fixed tune third, read by the window reader for the first time (`ship-uv`),
scored by `converge-s2/uv_served.py` unchanged. Passes if at least 15 of 44 cases serve the right current state,
beating the passes reader's 14/44 (CLOSING_BRIEF.md). No one-shot exists for uv, so best-in-class is could-not-judge
here. This bar only asks that the shipped reader beat the one it replaced.

**S4, a register of rules in force.** A register built from this repo's own rules (canon and operator directions
with their dates), under `register/`. Its documents, gold and scorer are committed before any read. One one-shot
read and one fresh system read; passes at the one-shot minus 2 on its scorer's main count.

**S5, the n+1 census.** Twelve owner descriptions (`census/descriptions/`), written before any authoring, from
outside the tuning examples (mail CRM, issue support, novels):
- recruiting, contracts, policies, purchasing, incidents, tasks;
- spare parts, lettings, grants, vans, expenses, a coliving house.

Each gets one blind author session under `blind-author/protocol.md`, with today's docs and today's validator. Each
numbered owner need is judged one of three ways:
- **runs:** declared with primitives that the reader, RESOLVE or a derivation executes, so a typed query can answer it;
- **inert:** declared, but nothing executes it (a validator warning, a key the parser drops, a documented-only facet);
- **inexpressible:** dropped, or held as free text where the owner needs a typed value, a link or a date.

Every miss is tagged with the primitive it lacks.
- **E1:** every use case's record types are declarable with running primitives, 12/12.
- **E2:** after building the primitives that two or more use cases lack, at least 90% of all owner needs run.
- **E3, the n+1 bar.** In seed-0 order, the last four use cases add no gap tag the first eight did not already show,
  judged on the first-pass recipes before any fix. Seed-0 order: contracts, vans, grants, tasks, expenses,
  policies, purchasing, lettings, incidents, recruiting, coliving, spare parts.

A gap that appears in one use case only is a stated limit of the claim, not a blocker.

**S6, the user's path.** The CLI journeys for mail, issues and news (`svrn/docs/cli-contract.toml`) pass on the
live daemon. A test asserts each fixture's `expected.toml` (record ids, values, evidence).

## Decision

Ship when S1-S6 pass. A failed bar is reported with its numbers. It blocks the claim for its shape, unless the
operator narrows the claim to exclude that shape.
