---
schema: work-order/v1
id: handed-9-custody-type
status: draft
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — custody: code cannot omit the policy
engine: ralph pool; REVIEW-mint first, then the rows it mints
budget: see the mint row's cap in ralph/next/handed/STATE.md
---

# Order: handed-9-custody-type — the policy is a type with one accessor

## Objective

hd-5 flips the serde default so a policy-less recipe parses private. That is a
default, and a second reader can still resolve absence its own way. This order
closes the code half at compile time: the policy becomes a type with no `Default`
impl, the field is private, and one accessor resolves absence — with a named warn,
not silently (principle 6). In-repo code that constructs a corpus meta without a
policy is E0063.

A recipe is DATA: no compile error can constrain what an author writes. The ceiling
for the data half is this accessor, or refusing at parse — the approval decision
recorded in campaign.md.

## Premises to verify before minting

- hd-5 has landed: `mesh_sharing` takes `#[serde(default)]`, the shipped recipes and
  the two generators state their values, and the `scope` doc comment is corrected.
- **All four `CorpusMeta` struct literals are OUT OF CRATE, which changes the error code
  and therefore the mechanism** (verified 2026-09-17, round 4: `git grep -nE 'CorpusMeta
  \{'` minus the definition at corpus-engine/src/recipe.rs:783): sovereign-tools
  knowledge_view/recipes.rs:41, :136, :260 and catalog_ingest.rs:860. corpus-engine
  itself has ZERO. A private field plus an out-of-crate literal is **E0451** ("field is
  private"), not E0063 ("missing field") — the repo already has the stderr for exactly
  this shape at kernel-types/tests/ui/citation_without_a_seal.stderr. So the mechanism is
  a CONSTRUCTOR: `CorpusMeta` gains one that takes the policy by value, the field goes
  private, and the four out-of-crate literals must call it. The plant is then E0451 at a
  literal (the field cannot be named from outside) or E0061 at the constructor (the
  policy argument removed) — the row names which one it runs, and the audit checks that
  code. What is NOT available is an in-crate E0063 plant: there is no in-crate literal to
  plant on.
- **Three sites an earlier draft listed are NOT E0063 sites and are not this rung's.**
  `render_governance_recipe` (sovereign-daemon governance_http.rs:942),
  `private_corpus_recipe_toml` (sovereign-server corpus_upload.rs:141) and the
  local_corpus template (local_corpus/config.rs:65) generate TOML *text* with `format!`
  — there is no struct literal for a missing field to be missing from. All three are
  already decided and priced by hd-5 (order-5 step 3), which this rung depends on. If
  the accessor changes what they must emit, that is a string edit in hd-5's files, not
  an E0063.
- Whether `query_sharing: Option<bool>` folds into the same type or stays: the mint
  decides from the readers (sovereign-mesh capabilities.rs ~:269).
- **The two SECOND readers this rung must absorb.** hd-5 flips neither, and both keep a
  `true` default, so "no second reader can resolve absence its own way" is false until
  each is either folded into the one accessor or NAMED as excluded — the mint decides
  which, per reader, and writes the decision down. Verified 2026-09-17:
  - `RegistryEntry.mesh_sharing` — corpus-engine/src/registry.rs:76-77,
    `#[serde(default = "default_true")]` at :76 over that crate's own `fn default_true()`
    (registry.rs:112). It reaches the desktop catalog: `Registry::catalog()`
    (registry.rs:311) maps `e.mesh_sharing` at :321 into `BuiltinCorpus`
    (corpus-engine/src/types.rs:674, the field at :681).
  - `CorpusDefinition.mesh_sharing` — a second manifest parser with its own private
    default: sovereign/crates/sovereign-tools/src/corpus/registry.rs:45-51,
    `#[serde(default = "default_mesh_sharing")]` at :45 over
    `fn default_mesh_sharing() -> bool { true }` at :49-51, which a
    `git grep default_true` cannot see.

## Cap basis (re-priced at round 3; cap 5 in STATE.md, was 3)

Measured 2026-09-17, corrected at round 4. The E0063 subject is small — `CorpusMeta`
struct literals are **4 sites / 2 files**; the "5 / 3" an earlier draft carried counted
`pub struct CorpusMeta {` at recipe.rs:783, the definition itself. The cost is the field going
private: `.mesh_sharing` is read at **29 sites / 15 files / 6 crates** (16 sites / 8
files inside corpus-engine/src, 13 / 7 outside), and the `IndexMeta` share of those
is OUT of scope — an installed index keeps its stamp (`[[ability]] custody`
`not_covered`), so resolve each access by receiver before repointing it.

1 row for the type, 2 for the accessor and the ~15 files of readers at ~10 a row, 1
for the 4 literals plus the decision on the two second readers above — **4 rows**. The
plant is NOT a row of its own: every landing row in this campaign carries its PLANT in
its `check:` list (the one standalone plant row, hd-6, is a rung whose whole content IS
the plant). Cap 5 leaves one row of slack. Past 5, PROMPT §4 applies: write `ralph/NEEDS_HUMAN.md` with
the measured count and stop. Do not count the 156 repo-wide `mesh_sharing` mentions
as the subject; most are `IndexMeta`, fixtures and serde attributes.

## Steps

1. Mint the policy type in corpus-engine beside `CorpusMeta`: no `Default`, no
   `Deserialize` shortcut that invents a value.
2. Make the field private with one accessor that resolves absence to private and
   warns once per corpus, naming the recipe.
3. Repoint the in-repo constructors (each must state a policy) AND every READER the private
   field breaks: `.mesh_sharing` is read at 29 sites / 15 files / 6 crates, 13 of those sites
   out of crate. Resolve each by receiver first — an `IndexMeta` read is out of scope (an
   installed index keeps its stamp) and must not be repointed.
4. PLANT: name `CorpusMeta`'s now-private policy field in one of the four out-of-crate
   literals -> LINT red **E0451**; or drop the policy argument at a call of the new
   constructor -> LINT red **E0061**. The row names which, and only that one, because the
   audit checks the code the row names. Revert, LINT green. E0063 is NOT available here:
   every literal is out of crate (Premises).
5. Do the mint's three queue duties (PROMPT §4): the two `depends` lists, a
   `conflicts.txt` pair per shared file, and `ralph.py report` proving the queue parses.

## Kill

- A reader needs the raw absent/present distinction that the accessor collapses:
  stop — that is a second decider (principle 8) and the shape is wrong.
