// SPDX-License-Identifier: AGPL-3.0-or-later
//! The field-model enrichment-domain gate — `Recipe::from_toml` refuses a
//! `type = "field_model"` recipe whose `domain` is not in the field-model
//! [`DomainRegistry`](corpus_engine::enrichment::domain_registry::DomainRegistry).
//!
//! **The failure this gate reproduces is not hypothetical.** On 2026-08-07 two
//! ingests died after building their entire index, ~90 minutes apart:
//!
//! ```text
//! 17:38:45Z WARN spawn_corpus_install: ingest failed
//!           corpus=brothers_karamazov error=Unknown enrichment domain: literary
//! 19:07:30Z WARN spawn_corpus_install: ingest failed
//!           corpus=brothers-karamazov-book-1 error=Unknown enrichment domain: literary
//! ```
//!
//! Both recipes said `type = "field_model"` with `domain = "literary"`.
//! `literary` names an atlas *pipeline* (`literary_atlas`), not a field-model
//! *domain* — two registries sharing one key name. The runtime check in
//! `FieldModelEngine::from_recipe` catches it, but only after acquire, extract,
//! embed and index have all run, so each failure stranded a fully-built
//! partition and no canonical corpus. Fixed per-recipe in `d88b4797`; this gate
//! is the structural version of that fix (§10 — make it structural, not
//! remembered).
//!
//! The two `observed_failure_*.toml` fixtures are the **byte-exact historical
//! recipes** (`git show ca97bb6f:…/brothers_karamazov/recipe.toml` and
//! `git show b175ff7d:…/brothers-karamazov-book-1/recipe.toml`), not
//! reconstructions. `fixed_atlas_literary.toml` is the same file at `d88b4797`,
//! i.e. the state that made the install work.

use std::path::PathBuf;

use corpus_engine::Recipe;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recipe_domain_gate")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {} unreadable: {e}", path.display()))
}

/// Load and return the error string, panicking if the recipe was ACCEPTED.
/// A gate that never rejects is not a gate (§18.1) — this helper makes
/// "it parsed fine" a loud failure rather than a silent pass.
fn expect_rejected(name: &str) -> String {
    match Recipe::from_toml(&fixture(name)) {
        Ok(r) => panic!(
            "fixture `{name}` (corpus `{}`) was ACCEPTED — the enrichment-domain \
             gate did not fire. This fixture is a verbatim copy of a recipe that \
             stranded a real ingest; if it loads, the gate is dead.",
            r.corpus.id
        ),
        Err(e) => e.to_string(),
    }
}

fn expect_accepted(name: &str) -> Recipe {
    Recipe::from_toml(&fixture(name)).unwrap_or_else(|e| {
        panic!("fixture `{name}` should load cleanly but was rejected: {e}")
    })
}

// ── The watched-to-fail cases ────────────────────────────────────────────

/// Failure #1, 17:38:45Z, `corpus=brothers_karamazov`.
#[test]
fn observed_failure_brothers_karamazov_is_rejected_at_load() {
    let msg = expect_rejected("observed_failure_brothers_karamazov.toml");
    assert!(
        msg.contains("brothers_karamazov"),
        "message must name the offending recipe; got: {msg}"
    );
    assert!(
        msg.contains("literary"),
        "message must name the offending domain (§18.3 — absence is reported, \
         never defaulted); got: {msg}"
    );
}

/// Failure #2, 19:07:30Z, `corpus=brothers-karamazov-book-1`. Same mistake,
/// different recipe — which is why the gate is structural and not a lint on
/// one file.
#[test]
fn observed_failure_brothers_karamazov_book_1_is_rejected_at_load() {
    let msg = expect_rejected("observed_failure_brothers_karamazov_book_1.toml");
    assert!(
        msg.contains("brothers-karamazov-book-1"),
        "message must name the offending recipe; got: {msg}"
    );
    assert!(
        msg.contains("literary"),
        "message must name the offending domain; got: {msg}"
    );
}

/// The failure message must carry the WHOLE valid set, not just "invalid".
/// An author who reads this message should never have to open
/// `domain_registry.rs` to learn what to type instead.
#[test]
fn rejection_names_every_registered_domain() {
    let msg = expect_rejected("observed_failure_brothers_karamazov_book_1.toml");
    for domain in [
        "philosophy",
        "personal",
        "conversational",
        "business_email",
        "institutional",
    ] {
        assert!(
            msg.contains(domain),
            "message must list the valid domain `{domain}`; got: {msg}"
        );
    }
}

/// The specific confusion that caused both failures — two registries, one key
/// name — must be named in the message, because "invalid domain" alone sends
/// the author looking for a typo in a word that is spelled correctly.
#[test]
fn rejection_points_at_the_atlas_type_the_recipe_actually_wanted() {
    let msg = expect_rejected("observed_failure_brothers_karamazov_book_1.toml");
    assert!(
        msg.contains("atlas"),
        "message should point at `type = \"atlas\"`, which is what `d88b4797` \
         changed to make this exact recipe install; got: {msg}"
    );
}

// ── The cases that must still pass ───────────────────────────────────────

/// The `d88b4797` fix itself. Same file, same `domain = "literary"`, only
/// `type` changed — proof the gate discriminates on the pair, not on the word
/// `literary`.
#[test]
fn the_atlas_fix_for_the_same_recipe_loads() {
    let recipe = expect_accepted("fixed_atlas_literary.toml");
    let enrichment = recipe.enrichment.expect("fixture has [enrichment]");
    assert_eq!(enrichment.enrichment_type, "atlas");
    assert_eq!(enrichment.domain.as_deref(), Some("literary"));
}

/// A registered field-model domain loads. One key differs from the rejected
/// fixture.
#[test]
fn field_model_with_registered_domain_loads() {
    let recipe = expect_accepted("good_field_model_registered_domain.toml");
    let enrichment = recipe.enrichment.expect("fixture has [enrichment]");
    assert_eq!(enrichment.enrichment_type, "field_model");
    assert_eq!(enrichment.domain.as_deref(), Some("philosophy"));
}

/// An absent `domain` is a real, working configuration — `from_recipe` falls
/// back to `philosophy`, which is registered. The gate judges only a
/// present-and-wrong domain, so omitting the key must not be an error.
#[test]
fn field_model_without_a_domain_loads() {
    let recipe = expect_accepted("good_field_model_no_domain.toml");
    let enrichment = recipe.enrichment.expect("fixture has [enrichment]");
    assert_eq!(enrichment.enrichment_type, "field_model");
    assert_eq!(enrichment.domain, None);
}

/// `enabled = false` never constructs the field-model engine, so a bad domain
/// there has no failing run behind it and must not fail the load. Guards
/// against widening the gate into recipes it cannot justify rejecting.
#[test]
fn disabled_enrichment_block_with_a_bad_domain_still_loads() {
    let recipe = expect_accepted("disabled_block_bad_domain.toml");
    let enrichment = recipe.enrichment.expect("fixture has [enrichment]");
    assert!(!enrichment.enabled);
    assert_eq!(enrichment.domain.as_deref(), Some("literary"));
}

// ── One decider ──────────────────────────────────────────────────────────

/// The gate reads its valid set from `DomainRegistry::builtin()` rather than
/// re-listing it, so registering a sixth domain widens the gate with no second
/// edit (§10.6). This test fails if someone hard-codes the list: it asserts the
/// message's set and the registry's set are the same set.
#[test]
fn valid_set_in_the_message_is_the_registry_itself() {
    use corpus_engine::enrichment::domain_registry::DomainRegistry;

    let msg = expect_rejected("observed_failure_brothers_karamazov_book_1.toml");
    let registered = DomainRegistry::builtin().domain_ids().len();

    // Every registered id appears...
    for id in DomainRegistry::builtin().domain_ids() {
        assert!(
            msg.contains(id),
            "registered domain `{id}` missing from the failure message: {msg}"
        );
    }
    // ...and the message lists exactly that many, comma-separated, so a
    // hard-coded list that drifts ahead of (or behind) the registry trips here.
    let listed = msg
        .split("Valid field-model domains are: ")
        .nth(1)
        .and_then(|tail| tail.split('.').next())
        .map(|list| list.split(", ").count())
        .expect("message should carry a `Valid field-model domains are: …` clause");
    assert_eq!(
        listed, registered,
        "message lists {listed} domains but the registry has {registered}; \
         the valid set was probably re-listed by hand instead of read from \
         DomainRegistry::builtin()"
    );
}

// ── The gate, applied to what actually ships ─────────────────────────────

/// **Every bundled recipe must survive its own loader.** This is the test that
/// would have caught the 2026-08-07 failures at `cargo test` time instead of at
/// a user's ingest, and it is the reason the gate is worth having: it turns
/// "someone will notice" into "the build says so".
///
/// `KNOWN_BROKEN` is a *reported absence*, not a suppression (§18.3). A recipe
/// listed here is one this gate rejects and that nobody has decided how to fix.
/// The list is asserted in both directions — a NEW rejection fails the test, and
/// so does FIXING a listed recipe without removing its row — so it can only
/// shrink deliberately and can never quietly grow.
#[test]
fn every_bundled_recipe_loads_except_the_ones_we_name() {
    use corpus_engine::recipe_builtin::{bundled_recipe_toml, RecipeId};

    /// **Empty, and that is the state to defend.** The list held exactly one
    /// row — `wikipedia-article`, which declared `type = "field_model"` with
    /// the unregistered `domain = "encyclopedic"` and so died at the end of
    /// every on-demand article ingest with `Unknown enrichment domain:
    /// encyclopedic`. The owner decision it was waiting on landed 2026-08-07:
    /// `enabled = false`, matching every sibling in the wikipedia family.
    ///
    /// Adding a row back is allowed but never free — it means a recipe ships
    /// broken, so a new row must carry the reason and name who has to decide,
    /// not just the id.
    const KNOWN_BROKEN: &[&str] = &[];

    // Worked example of what a row costs and how one gets retired, kept
    // because the next person tempted to add a row should read it first.
    //
    // The retired row said: `wikipedia-article` declares `type =
    // "field_model"` with `domain = "encyclopedic"`, which is not a registered
    // field-model domain — the same class of mistake as the two Brothers
    // Karamazov failures, so an on-demand article ingest builds its whole
    // index and then dies with `Unknown enrichment domain: encyclopedic`. It
    // is broken TODAY, at runtime; this gate only moves the failure earlier.
    // It is NOT fixed here because the correct value is a product decision,
    // and there are two defensible answers — `enabled = false` (what every
    // sibling in the wikipedia family does) or `type = "atlas"` with the
    // `referential_atlas` pipeline. Owner decision required.
    //
    // Ratified 2026-08-07: `enabled = false`. The row lived for exactly as
    // long as the decision was open, which is the only honest lifetime for
    // one — a row that outlives its decision is a suppression wearing a
    // reason.

    let mut rejected = Vec::new();
    for id in RecipeId::ALL {
        let toml = bundled_recipe_toml(id.id())
            .unwrap_or_else(|| panic!("bundled recipe `{}` is missing", id.id()));
        if let Err(e) = Recipe::from_toml(toml) {
            rejected.push((id.id(), e.to_string()));
        }
    }

    for (id, err) in &rejected {
        assert!(
            KNOWN_BROKEN.contains(id),
            "bundled recipe `{id}` no longer loads and is not a known-broken \
             entry. Either fix the recipe or add it to KNOWN_BROKEN with the \
             reason. Loader said: {err}"
        );
    }
    for known in KNOWN_BROKEN {
        assert!(
            rejected.iter().any(|(id, _)| id == known),
            "`{known}` is listed as known-broken but now loads cleanly — delete \
             its KNOWN_BROKEN row so the list keeps meaning what it says"
        );
    }
}
