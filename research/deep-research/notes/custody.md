# Custody schema + join rule — design note (order `deep-research-t0b`, reds R-2/R-3/R-4)

**Status:** design only. The three reds in
`sovereign/crates/sovereign-core/tests/custody_reds.rs` are the
specification in failing-test form — each compiles at HEAD, fails when
run, and fails for the attributed reason. T1's job is to make the
assertions true, not to rewrite them. **No custody code exists anywhere
at this commit** — no enum, no stamp, no refusal. This note is the
design T1 lands.

## §1 The custody enum — closed set, one name

A chunk's custody is one of:

| class | meaning |
|---|---|
| `public-web` | fetched from the open web (a URL fetch, a crawl, an RSS item, a published page) |
| `personal` | the estate's own material — local files, notes, conversations, imported personal data |
| `peer` | material that arrived from another node on the mesh (peer corpora, shared indexes, gossip-carried content) |

Closed set → an enum, never a stringly registry (ARCH §2). The reds
spell the values as `"public-web" | "personal" | "peer"` (custody_reds.rs
`CUSTODY_CLASSES`); the enum's `to_string` must produce exactly those
wire spellings so the released metadata is stable.

**Unknown provenance is a THIRD VARIANT, not a default.** A chunk whose
provenance cannot be determined is not quietly `personal` or `public-web`
— it is `unknown`, and `unknown` **refuses** (§4). The defect at HEAD is
precisely that unknown defaults to Leaf (grounding/mod.rs:248-253,
`source_of(idx)` `unwrap_or(Leaf)`) — an unstamped chunk grounds a
factual claim like any other. R-3 asserts the refusal.

## §2 Stamp sites — the fetcher, never a model task

Custody is stamped **at acquisition**, by the code path that brings the
material in — never by a model, never inferred later by the gate.

- Web fetches: the fetcher stamps `public-web` + the **source URL** as
  the chunk is written. The URL is a first-class field of the chunk's
  provenance record from the moment of creation.
- Estate ingestion (local files, notes, the corpus ingester): stamps
  `personal` (and the estate path, for the release record).
- Mesh arrival: the node that accepts a peer's material stamps `peer`,
  preserving the originating node id alongside.

Why the fetcher: custody is a fact about *where the bytes came from*,
known only at the acquisition boundary. Any later inference is a model
asked to guarantee a fact code already knew (ARCH §7.6 — never ask a
model to guarantee what code can enforce). The harness's
`ingest_test_corpus` is the estate-side stand-in for the stamp site; the
web-fetch leg is the same code path in production (the hand-run recorded
DDG as the fetch surface; headless fetching has the same seam).

**The defect R-2 asserts:** the fetcher stamps nothing — no custody, no
URL — so a fetched chunk arrives at the gate with `url: null` and no
custody class, and the released record shows it (`retrieved_chunks[].url`
is `null` at HEAD — the red's failing assertion). The fix: the stamp
travels with the chunk through `EvidenceContext` into the gate and onto
the released record.

## §3 Derived custody — max-restrictiveness join, computed at creation

A tier-2 summary (or any derived artifact — a digest, a spliced
passage, a multi-chunk synthesis) is assembled from inputs that may
carry different custodies. Its own custody is the **max-restrictiveness
join over its derivation inputs**, computed **at creation** — not at
egress time, not by a later pass:

```
join(inputs) = personal  if any input is personal
             | peer      else if any input is peer
             | public-web otherwise
```

Rationale: a summary built from a public-web passage and a personal file
is *at least as sensitive as its most sensitive input* — content is
blended, so the summary must obey the strictest class. "Computed at
creation" means the join result is a property of the artifact, carried
alongside it; egress checks key on it without re-deriving (and without
needing the original inputs at egress time). R-4 asserts that a
mixed-custody summary's derived custody rides the release — the red
fails by construction at HEAD because no custody value exists anywhere
for the stub egress check to key on (custody_reds.rs `derive_custody` +
`egress_refuses` are the specification in code).

## §4 Unknown provenance refuses

When the gate assembles evidence and a chunk's provenance is unknown
(no stamp, no derivable join — the sealed/pinned evidence paths, where
chunks are appended after the evidence builder ran and have no source
row), the gate **may not release a factual claim resting on it**: the
action must be a refusal (`abstained_*` / `refused_*` family), and the
gate meta must say why (the `provenance_class: "unknown"` entry in the
per-chunk ledger).

Today unknown→Leaf (grounding/mod.rs:248-253): the chunk grounds like
any estate chunk. That is the R-3 defect. The refusal is
fail-closed by construction — no custody, no grounding — and it is the
inverse of every fail-open in this codebase's history (ARCH §18.3):
the unknown case is reported, never defaulted.

## §5 What rides the released surface

The reds assert on three released-surface keys (all absent at HEAD —
that is the red):

| key | surface | content |
|---|---|---|
| `url` | `retrieved_chunks[]` | the chunk's source URL (non-null for every fetched chunk) |
| `custody` | `retrieved_chunks[]` | the chunk's custody class (one of the enum; for a derived summary, the §3 join) |
| `chunk_custody` | `grounding_gate` meta | the per-chunk ledger the judge saw: `{locator, custody, provenance_class: known\|unknown, source_url}` |

The gate's own ledger (`chunk_custody`) is the judge-side record — the
evidence the verdict was computed over — so a released answer can be
audited against what the gate saw. `provenance_class` distinguishes
`unknown` (refusal trigger, §4) from the three stamped classes.

## §6 Egress — the consumer that makes custody load-bearing

Custody exists to be keyed on at the boundaries where content leaves the
estate: clipboard, export, mesh share, attached-doc send. The rule (the
reds' `egress_refuses`): **a summary or excerpt may leave only when its
custody is `public-web`** — `personal` and `peer` material stays in the
estate unless the operator's own surface overrides with a reason
recorded. The reds' stub is the specification; T1's egress surfaces key
on the released `custody` field, never on a re-derivation.

## §7 Map: reds → design

| red | defect at HEAD | design that makes it green |
|---|---|---|
| R-2 | fetched chunk carries no custody/URL; released `url` is null | §2 stamp at acquisition; stamp rides `EvidenceContext` → gate → release |
| R-3 | unstamped chunk grounds; unknown→Leaf (mod.rs:248-253) | §4 `provenance_class: unknown` in the gate ledger → refusal action |
| R-4 | mixed-custody summary has no custody to key on | §3 join at creation; derived custody rides the release; §6 egress keys on it |

## §8 Scope guard

T1 touches: the fetcher/ingester stamp sites, `EvidenceContext`
(grounding/mod.rs — the crate-internal shape), the gate's meta assembly,
and the egress surfaces. The custody enum is new code; the released
metadata keys are additive. The reds' fixture trajectories (documented
per test) bind T1 to exposing: the stamp on the harness ingest path, and
an unstamped (sealed/pinned) evidence path for R-3's refusal assertion.
