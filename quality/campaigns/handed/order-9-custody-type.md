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
- The in-repo constructors of the corpus meta: the KnowledgeView recipe literals
  (sovereign-tools knowledge_view/recipes.rs), governance_http.rs:942-956,
  sovereign-server/src/corpus_upload.rs:141, the local_corpus template.
- Whether `query_sharing: Option<bool>` folds into the same type or stays: the mint
  decides from the readers (sovereign-mesh capabilities.rs ~:269).

## Steps

1. Mint the policy type in corpus-engine beside `CorpusMeta`: no `Default`, no
   `Deserialize` shortcut that invents a value.
2. Make the field private with one accessor that resolves absence to private and
   warns once per corpus, naming the recipe.
3. Repoint the in-repo constructors; each must state a policy.
4. PLANT: construct a corpus meta literal without the policy; LINT reports E0063.
   Revert, LINT green.

## Kill

- A reader needs the raw absent/present distinction that the accessor collapses:
  stop — that is a second decider (principle 8) and the shape is wrong.
