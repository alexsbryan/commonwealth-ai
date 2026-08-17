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
    let recipe_path = recipes_dir.join(corpus_id).join("recipe.toml");
    let declared = std::fs::read_to_string(&recipe_path)
        .ok()
        .and_then(|s| crate::Recipe::from_toml(&s).ok())
        .and_then(|r| r.authority)
        .is_some_and(|a| a.tool == SEC_FACTS_AUTHORITY_TOOL);
    if !declared {
        tracing::debug!(target: "sec_facts",
            corpus_id = %corpus_id, recipe = %recipe_path.display(),
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
