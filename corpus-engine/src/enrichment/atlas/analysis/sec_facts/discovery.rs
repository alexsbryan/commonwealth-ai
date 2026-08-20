// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which installed corpora DECLARE the `sec_facts` typed store
//! authoritative — FINANCIAL_CORPORA.md §7.3.
//!
//! One implementation, called by every consumer (ARCH §10.6): the
//! `sec_facts` tool's claim index and the desktop coverage card both
//! resolve through here, so a corpus cannot be answerable by one and
//! invisible to the other.

use super::{SecFactStore, SEC_FACTS_SIDECAR};

// ---------------------------------------------------------------------
// Discovery — ONE implementation, called by every consumer (§10.6)
// ---------------------------------------------------------------------

/// The tool id a recipe names in `[authority]` to declare this typed
/// store authoritative for its corpus. One name (§10.6).
pub const SEC_FACTS_AUTHORITY_TOOL: &str = "sec_facts";

/// Read the typed store for `corpus_id` IF its recipe declares
/// `[authority] tool = "sec_facts"` and the sidecar is present.
///
/// Authority is DECLARED by the recipe author (§7.3); a sidecar alone
/// never grants it, and the corpus id's SPELLING never enters into it. A
/// `sec-cik…` name prefix is an ADDRESS, not an essence (ARCH §7.5) — it
/// breaks silently the day the id convention changes, and it was never
/// the real predicate.
pub fn authoritative_store(
    index_dir: &std::path::Path,
    recipes_dir: &std::path::Path,
    corpus_id: &str,
) -> Option<SecFactStore> {
    let sidecar = index_dir.join(corpus_id).join(SEC_FACTS_SIDECAR);
    if !sidecar.exists() {
        return None;
    }
    // TWO SOURCES FOR THE RECIPE, BOTH LIVE, DISK FIRST — and the next
    // reader needs to know why both exist.
    //
    // The predicate is unchanged: the recipe AUTHOR declares `[authority]`
    // (§7.3). Only the LOOKUP changed. Disk-only was silently wrong for
    // every corpus installed from the CATALOG: that path resolves the
    // recipe through the registry (bundled, or fetched from `toml_url`)
    // and never writes it to `recipes_dir`. Only `setup-sec-corpus.sh`
    // materializes a recipe on disk, so "works from the script, refuses
    // from the catalog" was the observable symptom — the desktop coverage
    // card returned null for a store holding 20 concepts and a valid
    // as-of (order `sec-filings-last-mile`, e2e attempt 4).
    //
    // Disk is checked FIRST so a user override still wins outright,
    // including an override that REMOVES the declaration — the compiled-in
    // copy must never resurrect authority the user's own file dropped.
    // The bundled copy is consulted only when nothing is on disk.
    //
    // A sidecar STILL never grants authority on its own: an id with no
    // resolvable recipe, or one whose recipe declares nothing, is refused
    // exactly as before.
    let recipe_path = recipes_dir.join(corpus_id).join("recipe.toml");
    let from_disk = std::fs::read_to_string(&recipe_path)
        .ok()
        .and_then(|s| crate::Recipe::from_toml(&s).ok());
    let bundled = from_disk.is_none();
    let recipe = from_disk.or_else(|| {
        crate::recipe_builtin::bundled_recipe_toml(corpus_id)
            .and_then(|toml| crate::Recipe::from_toml(toml).ok())
    });
    let declared = recipe
        .and_then(|r| r.authority)
        .is_some_and(|a| a.tool == SEC_FACTS_AUTHORITY_TOOL);
    if !declared {
        tracing::debug!(target: "sec_facts",
            corpus_id = %corpus_id, recipe = %recipe_path.display(),
            source = if bundled { "bundled" } else { "on-disk" },
            "sec_facts: sidecar present but recipe declares no \
             [authority] tool = \"sec_facts\" — not authoritative for it");
        return None;
    }
    match std::fs::read_to_string(&sidecar)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str::<SecFactStore>(&s).map_err(|e| e.to_string()))
    {
        Ok(store) => {
            tracing::debug!(target: "sec_facts",
                corpus_id = %corpus_id, entity = %store.entity,
                concepts = store.concepts.len(),
                "sec_facts: loaded declared-authoritative typed store");
            Some(store)
        }
        Err(err) => {
            tracing::warn!(target: "sec_facts",
                corpus_id = %corpus_id, error = %err,
                "sec_facts: unreadable sidecar — excluded");
            None
        }
    }
}

/// Every installed corpus this typed store is DECLARED authoritative for,
/// id-sorted for determinism.
///
/// The single discovery rule: the `sec_facts` tool's claim index and the
/// desktop coverage card both call this, so a corpus can never be
/// answerable by one and invisible to the other (§10.6).
pub fn discover_authoritative_stores(
    index_dir: &std::path::Path,
    recipes_dir: &std::path::Path,
) -> Vec<(String, SecFactStore)> {
    let Ok(entries) = std::fs::read_dir(index_dir) else {
        tracing::debug!(target: "sec_facts", index_dir = %index_dir.display(),
            "sec_facts: index dir unreadable — no authoritative corpora");
        return Vec::new();
    };
    let mut out: Vec<(String, SecFactStore)> = Vec::new();
    for e in entries.flatten() {
        let corpus_id = e.file_name().to_string_lossy().to_string();
        if let Some(store) = authoritative_store(index_dir, recipes_dir, &corpus_id) {
            out.push((corpus_id, store));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    tracing::debug!(target: "sec_facts", declared = out.len(),
        "sec_facts: discovery complete");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::analysis::sec_facts::fixtures::store;

    // -----------------------------------------------------------------
    // Discovery (§7.3, ARCH §7.5) — declared authority, never the name
    // -----------------------------------------------------------------

    fn write_corpus(
        index_dir: &std::path::Path,
        recipes_dir: &std::path::Path,
        corpus_id: &str,
        authority_tool: Option<&str>,
    ) {
        let idx = index_dir.join(corpus_id);
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(
            idx.join(SEC_FACTS_SIDECAR),
            serde_json::to_string(&store()).unwrap(),
        )
        .unwrap();
        let rec = recipes_dir.join(corpus_id);
        std::fs::create_dir_all(&rec).unwrap();
        let authority = match authority_tool {
            Some(t) => format!("\n[authority]\ntool = \"{t}\"\n"),
            None => String::new(),
        };
        std::fs::write(
            rec.join("recipe.toml"),
            format!(
                "[corpus]\nid = \"{corpus_id}\"\nname = \"t\"\ndescription = \"d\"\n\
                 license = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"p\"\n\n\
                 [extract]\ntype = \"plaintext\"\n\n[chunk]\ntype = \"fixed\"\n\
                 max_chars = 3000\n{authority}"
            ),
        )
        .unwrap();
    }

    /// ARCH §7.5 regression guard, and the defect M1b removed: discovery
    /// keys on the DECLARED authority, never on the corpus id's spelling.
    /// Both halves matter — a differently-named corpus that declares must
    /// be found, and a `sec-cik…`-named corpus that does not declare must
    /// not be.
    #[test]
    fn discovery_keys_on_declared_authority_not_the_corpus_name() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();

        // Declares, but is named nothing like `sec-cik…`.
        write_corpus(&index_dir, &recipes_dir, "acme-filings", Some("sec_facts"));
        // Named `sec-cik…`, sidecar present, but declares a DIFFERENT
        // authority — a name prefix would wrongly claim this one.
        write_corpus(
            &index_dir,
            &recipes_dir,
            "sec-cik0000320193",
            Some("some_other_tool"),
        );
        // Named `sec-cik…`, sidecar present, declares nothing at all.
        write_corpus(&index_dir, &recipes_dir, "sec-cik0000789019", None);

        let found = discover_authoritative_stores(&index_dir, &recipes_dir);
        let ids: Vec<&str> = found.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["acme-filings"],
            "discovery must follow [authority], not the id spelling"
        );
        assert!(authoritative_store(&index_dir, &recipes_dir, "acme-filings").is_some());
        assert!(authoritative_store(&index_dir, &recipes_dir, "sec-cik0000320193").is_none());
        assert!(authoritative_store(&index_dir, &recipes_dir, "sec-cik0000789019").is_none());
    }

    /// Write ONLY the sidecar — no recipe on disk. This is exactly what a
    /// CATALOG install leaves behind: the installer resolves the recipe
    /// through the registry (bundled or fetched) and never writes it to
    /// `recipes_dir`, which only the script path materializes.
    fn write_catalog_installed(index_dir: &std::path::Path, corpus_id: &str) {
        let idx = index_dir.join(corpus_id);
        std::fs::create_dir_all(&idx).unwrap();
        std::fs::write(
            idx.join(SEC_FACTS_SIDECAR),
            serde_json::to_string(&store()).unwrap(),
        )
        .unwrap();
    }

    /// THE CATALOG-INSTALL DEFECT (order `sec-filings-last-mile`, found by
    /// e2e attempt 4): a corpus installed from the catalog has a sidecar
    /// and NO recipe on disk, so a disk-only authority lookup refused it —
    /// and refused EVERY catalog install, for every consumer of this
    /// function. Measured: the desktop coverage card returned null for a
    /// corpus whose store held 20 concepts and a valid as-of.
    #[test]
    fn a_catalog_installed_corpus_is_authoritative_without_a_recipe_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();

        // `sec-filings-company` is bundled and its recipe declares
        // `[authority] tool = "sec_facts"`. Nothing is written to
        // `recipes_dir` — that is the whole point.
        write_catalog_installed(&index_dir, "sec-filings-company");
        assert!(
            !recipes_dir.join("sec-filings-company").exists(),
            "the fixture must NOT write a recipe to disk"
        );

        assert!(
            authoritative_store(&index_dir, &recipes_dir, "sec-filings-company").is_some(),
            "a catalog-installed corpus must resolve its declaration through the \
             same registry the installer used"
        );
        let ids: Vec<String> = discover_authoritative_stores(&index_dir, &recipes_dir)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, vec!["sec-filings-company".to_string()]);
    }

    /// THE OTHER HALF, and the one that keeps §7.3 intact: falling back to
    /// the bundled recipe must NOT become "a sidecar grants authority".
    /// `wikipedia` is bundled and declares NO `[authority]`; a sidecar
    /// beside it must still be refused. If this ever goes green, the
    /// declaration check has been traded for a file-existence check.
    #[test]
    fn a_sidecar_alone_never_grants_authority_even_with_a_bundled_recipe() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();

        // Bundled, but declares no authority.
        write_catalog_installed(&index_dir, "wikipedia");
        // Not a bundled recipe at all, and nothing on disk either.
        write_catalog_installed(&index_dir, "not-a-bundled-recipe-xyz");

        assert!(
            authoritative_store(&index_dir, &recipes_dir, "wikipedia").is_none(),
            "a bundled recipe that declares nothing must still be refused"
        );
        assert!(
            authoritative_store(&index_dir, &recipes_dir, "not-a-bundled-recipe-xyz").is_none(),
            "an unresolvable recipe must be refused, never defaulted to authoritative"
        );
        assert!(discover_authoritative_stores(&index_dir, &recipes_dir).is_empty());
    }

    /// Disk WINS over bundled, in both directions. The script path
    /// (`setup-sec-corpus.sh`) materializes a recipe on disk and must keep
    /// resolving; and a user override that REMOVES the declaration must be
    /// honoured rather than silently overridden by the compiled-in copy.
    #[test]
    fn an_on_disk_recipe_shadows_the_bundled_one() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();

        // Same id as the bundled declaring recipe, but the on-disk copy
        // declares nothing. The user's file is the effective recipe.
        write_corpus(&index_dir, &recipes_dir, "sec-filings-company", None);
        assert!(
            authoritative_store(&index_dir, &recipes_dir, "sec-filings-company").is_none(),
            "an on-disk recipe that declares no authority must not be rescued \
             by the bundled copy"
        );

        // And the script path's own shape still resolves: a CIK-keyed id
        // that exists only on disk, declaring authority.
        write_corpus(
            &index_dir,
            &recipes_dir,
            "sec-cik0000320193",
            Some("sec_facts"),
        );
        assert!(
            authoritative_store(&index_dir, &recipes_dir, "sec-cik0000320193").is_some(),
            "the script path must not regress"
        );
    }

    /// A declared recipe with no sidecar installed yet is not
    /// authoritative — absence is reported, never defaulted (ARCH §18.3).
    #[test]
    fn declared_without_a_sidecar_is_not_authoritative() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes");
        let recipes_dir = tmp.path().join("recipes");
        std::fs::create_dir_all(index_dir.join("declared-no-sidecar")).unwrap();
        let rec = recipes_dir.join("declared-no-sidecar");
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(
            rec.join("recipe.toml"),
            "[corpus]\nid = \"declared-no-sidecar\"\nname = \"t\"\ndescription = \"d\"\n\
             license = \"x\"\n\n[acquire]\ntype = \"local_file\"\npath = \"p\"\n\n\
             [extract]\ntype = \"plaintext\"\n\n[chunk]\ntype = \"fixed\"\nmax_chars = 3000\n\n\
             [authority]\ntool = \"sec_facts\"\n",
        )
        .unwrap();
        assert!(discover_authoritative_stores(&index_dir, &recipes_dir).is_empty());
    }
}
