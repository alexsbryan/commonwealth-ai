---
schema: work-order/v1
id: handed-5-custody
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: structural — custody: a recipe that states no sharing policy is private
engine: ralph pool; one REVIEW-build row (the stronger model), no HUMAN row
budget: 1 row, ~6 files; PLANT is a TEST plant; covered by the hd-5 REVIEW-audit
---

# Order: handed-5-custody — absent means private, and every generator says what it means

## Objective

A corpus's recipe decides whether the corpus may leave the node. Today a recipe that says nothing
means *shared*: `#[serde(default = "default_true")]` on `CorpusMeta.mesh_sharing`
(corpus-engine/src/recipe.rs:790). One attribute change makes absence mean private, and in the same
commit every recipe and generator that currently relies on the default states its value explicitly,
so the flip changes nothing for anything shipped and changes exactly one thing for everything
generated. The `scope` field's doc comment, which claims the privacy guarantee is "structural, not
policy-layer" when nothing in production reads the field, is corrected in the same commit.

Cut at round 1 (2026-09-17), 5 rows to 1: no `SharingPolicy` type is minted (it buys no plant the
tree lacks — `mesh_sharing: bool` is already non-`Option`, so an E0063 on a literal is red today),
`scope` is NOT deleted (deleting a field nobody reads is a cleanup, not this promise), and the HUMAN
row is an approval-time decision on three data values, answered in the Premises below.

## Premises (verified 2026-09-17, file:line)

- `pub struct CorpusMeta` — corpus-engine/src/recipe.rs:783. `#[serde(default = "default_true")]`
  :790, `pub mesh_sharing: bool` :791. `fn default_true()` :68.
- The `scope` doc comment is :792-797, and :796 reads "so the privacy guarantee is structural, not
  policy-layer." `#[serde(default)]` :798, `pub scope: Option<String>` :799. No production reader:
  `git grep -nE 'corpus\.scope'` hits only corpus-engine/tests/main/recipe_back_compat.rs:178,
  sovereign-tools knowledge_view/recipes.rs:353,438,471 (test module from :344) and
  local_corpus/config.rs:473,523 (test module from :200). (`meta.scope` at index/create.rs:520,527
  is `IndexMeta.scope: ScopeMeta`, a different field.) The comment is therefore a checkable claim
  that is false — principle 3's "comments must be checkable, or they rot invisibly".
- `pub query_sharing: Option<bool>` :816, doc :800-815: `None` means "fall back to `mesh_sharing`".
  With `#[serde(default)]` on `mesh_sharing`, a recipe stating neither key resolves private through
  the existing fallback — no second change is needed.
- **Tracked recipe TOMLs that state nothing: exactly 2.** tomllib over `git ls-files '*.toml'`,
  counting tables with `[corpus] id` and no `mesh_sharing`: `sovereign-recipes/maple-house/recipe.toml`
  and `sovereign/crates/sovereign-desktop/src-tauri/resources/starter/recipe.toml`. Both are shipped
  as shared today purely by the default.
- **Production generators that state nothing: exactly 2**, and the tracked-TOML census above cannot
  see either.
  - `render_governance_recipe` — sovereign-daemon/src/governance_http.rs:942 (fn decl), `[corpus]`
    block :952-956. Its value is read at sovereign-api routes_internal/corpus_collaborate.rs:92
    (`if !recipe_privacy.corpus.mesh_sharing`), which is the ONE production path that resolves
    custody from the recipe rather than from stamped index metadata.
  - `private_corpus_recipe_toml` — sovereign-server/src/corpus_upload.rs:141 (fn decl), `[corpus]`
    block at :143-145. It is named "private", its own doc says isolation is "the `CorpusState`
    ceiling, not the engine flag", and it parses `mesh_sharing = true` today. This is sufficiency
    finding 11's catch: its `[corpus]` header sits on the same source line as `format!(r#"`, so
    every census that greps `^\[corpus\]` misses it.
- **Every other production generator already states a value** — verified by taking every `*.rs`
  containing `[acquire]` and checking each: sovereign-cli/src/project_init/mod.rs:474 block,
  `mesh_sharing = false` :479; sovereign-cli-dev/src/drift_cmd_orchestrator.rs:804 block,
  `mesh_sharing = true` :814; sovereign-cli-llm/src/alignment_cmd.rs:59 `BUNDLED_RECIPE`,
  `mesh_sharing = true` :72; sovereign-cli-shared/src/code_index.rs:408 block,
  `mesh_sharing = false`; sovereign-tools/src/local_corpus/config.rs:136 template,
  `mesh_sharing = false` :141. `sovereign-contracts/src/recipe/json_to_toml.rs` copies tables
  generically and invents no key.
- **The decided values** (approval-time, recorded here so no HUMAN row is needed):
  - governance_http.rs → `mesh_sharing = false`. A user's governance folder is private state; today
    it collaborates freely without a grant (corpus_collaborate.rs:92) purely because nobody typed a
    key. This is the one BEHAVIOUR change in the row and it must be named in the commit body.
  - corpus_upload.rs → `mesh_sharing = false`. No behaviour intent change — the generator is already
    called "private"; this makes the file say what the function name says.
  - maple-house and the desktop starter → `mesh_sharing = true`. Both ship shared today; preserving
    the value is what makes "the flip changes nothing for shipped data" true.
- No `deny_unknown_fields` in recipe.rs, and the TOML key spelling does not change, so an older
  binary reading a newer recipe and a newer binary reading an older recipe both still parse. (The
  behaviour differs — that is the point — but nothing fails to load.)
- Back-compat test that must stay green: corpus-engine/tests/main/recipe_back_compat.rs (reads
  sovereign-recipes/wikipedia-newsworthy at :172, asserts sharing at :177).
- `CorpusMeta` test module: corpus-engine/src/recipe.rs:2127.
- `sovereign-recipes/SCHEMA.md` is generated by `recipe_schema_is_fresh`
  (corpus-engine/tests/main/recipe_schema.rs:77). The serde default text for `mesh_sharing` may be
  rendered there; bless with `UPDATE_RECIPE_SCHEMA=1`.

## Steps

1. One commit — `REVIEW-build-hd-5-default-private`: the attribute flip, the four explicit values,
   the corrected `scope` doc comment, the pinning test, the PLANT. There is no step 2.

## Seams

- **Must not touch.** `IndexMeta.mesh_sharing` (corpus-engine/src/index/mod.rs:323) and
  `IndexMeta.query_sharing` (:327) — those are the persistence and wire shapes replication ships,
  and an installed index keeps what it was stamped with; `IndexInfo`; the `grantable` field
  (already non-optional, already defaults false); `CorpusMeta.scope` itself (only its comment);
  `RegistryEntry.mesh_sharing` (corpus-engine/src/registry.rs:76) and
  `CorpusDefinition.mesh_sharing` (sovereign-tools/src/corpus/registry.rs:45), neither of which is
  an egress decider; the enforcement sites that read INDEX metadata (sovereign-mesh
  capabilities.rs:269, sovereign-api corpus_ingest.rs:285, sovereign-daemon daemon.rs:3926,
  sovereign-server routes.rs:559) — the flip does not reach them and must not.
- **hd-3 (writers)** under its CUT design touches no file this row touches. The
  `engine/ingest.rs` / `harness/runner.rs` overlap that the 25-row hd-3 shape had is gone.
- The 64 `[corpus]` tables in `#[cfg(test)]` modules and `tests/` that state no `mesh_sharing`
  become private under the flip. Any test asserting `mesh_sharing == true` on one of them goes red
  and is fixed in this row by stating the key in the fixture, never by reverting the default —
  this is the expected shape of the row's fallout and is not a §6.
- `sovereign/SYSTEM_OVERVIEW.md` is edited by every rung and is dirty on the tree now; the peer's
  hunks must be committed before this row's commit.

## Done when

- **PLANT red, gate green:** restore `#[serde(default = "default_true")]` on `mesh_sharing`
  (recipe.rs:790) → TEST(corpus-engine) red on `a_recipe_without_a_sharing_policy_is_private`;
  `git checkout --` the plant → green. The audit re-runs it and checks the failing assertion is that
  test's, not an unrelated one.
- **Ambient path gone**, each grep empty:
  `grep -n 'default = "default_true"' corpus-engine/src/recipe.rs` → no hit on a sharing field
  `git grep -n 'structural, not policy-layer' -- corpus-engine/src/recipe.rs` → empty
  the tracked-TOML census prints `missing mesh_sharing: 0`
  `git grep -nA6 'fn render_governance_recipe\|fn private_corpus_recipe_toml' | grep -c mesh_sharing` → 2
- LINT, TEST(corpus-engine), TEST(sovereign-daemon), TEST(sovereign-server), TEST(sovereign-api)
  exit 0.

**The promise this rung actually makes, narrowed.** *A corpus whose recipe states no sharing policy
is private, and no recipe or generator in the tree relies on the default.* What that does NOT cover,
each named rather than implied:

- **Prompt content routed to a peer is not covered.** `select_peers_ranked`
  (sovereign-serving-host/src/peer_inference.rs:1456) ships a completion — including retrieved
  corpus text in the prompt — to another node, and nothing in sovereign-serving-host,
  sovereign-inference or the commonwealth crates reads `mesh_sharing`, `query_sharing` or
  sensitivity (`git grep -c` over those three trees returns hits only in commonwealth's own `.md`
  files). That is the campaign's excluded `egress`/`model-reach`, and it is this promise's own
  subject: a private corpus's content can still leave the node inside a prompt.
- **Already-installed indexes keep their stamped value.** `IndexMeta.mesh_sharing`
  (corpus-engine/src/index/mod.rs:323) is written at ingest and read thereafter, so every corpus
  built before this commit keeps the `true` it was stamped with until it is re-ingested. Peer
  advertisement (sovereign-mesh capabilities.rs:269) reads the stamp, not the recipe, so the flip
  changes nothing there. The flip reaches exactly one production decider today —
  corpus_collaborate.rs:92 — plus every future ingest.
- **User recipes on disk change meaning.** An installed user recipe that omits the key was shared
  and becomes private. That is the intended direction (a custody default that errs toward silence),
  and it is stated here so it is a decision rather than a discovery.
- **Authored recipes.** The studio's JSON schema
  (studio/crates/sovereign-recipe-author/src/recipe_schema.rs:154-155) lists both keys and requires
  neither, so an authored recipe that names neither is now private. No change is made there.

## Kill

- A production reader of `CorpusMeta.mesh_sharing` exists that this order's census missed, and it
  decides egress for ALREADY-BUILT indexes (not just fresh ingests). Stop: the flip then changes
  behaviour for installed corpora beyond what this order priced.
- More than three shipped recipes or generators turn out to depend on the `true` default in a way
  that makes "the flip changes nothing for shipped data" false. Stop: the change needs a migration,
  which is a different rung.
