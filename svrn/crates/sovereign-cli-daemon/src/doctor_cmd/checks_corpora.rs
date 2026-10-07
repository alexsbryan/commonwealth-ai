// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether each code corpus's recorded source tree still exists
//! (code-intel-repo-scope: the closure that would have surfaced the
//! `svrngs-plant` audit copy).

use super::{CheckResult, CheckStatus, Layer, Repair};

/// A code corpus answers about the tree it was built from. One whose tree is
/// gone (an audit copy under /tmp, a deleted checkout) still answers every
/// unscoped code question, about code nobody has; one that records no tree
/// cannot be scoped to from its repo, so `svrn tools call` there searches
/// every corpus. Either is closed by a person, so this names both.
pub(super) fn check_code_corpus_sources() -> CheckResult {
    code_corpus_source_findings(&super::checks_freshness::orphaned_indexes(), |p| {
        std::path::Path::new(p).exists()
    })
}

/// [`check_code_corpus_sources`] over `corpora` (`(id, source_path)`), with
/// `exists` the filesystem.
fn code_corpus_source_findings(
    corpora: &[(String, Option<String>)],
    exists: impl Fn(&str) -> bool,
) -> CheckResult {
    let name = "code_corpus_sources";
    if corpora.is_empty() {
        return CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: "no code corpora indexed".into(),
            repair: Repair::None,
        };
    }
    let gone: Vec<&(String, Option<String>)> = corpora
        .iter()
        .filter(|(_, src)| src.as_deref().is_some_and(|p| !exists(p)))
        .collect();
    let unrecorded: Vec<&str> = corpora
        .iter()
        .filter(|(_, src)| src.is_none())
        .map(|(id, _)| id.as_str())
        .collect();
    if gone.is_empty() && unrecorded.is_empty() {
        return CheckResult {
            name,
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!(
                "all {} code corpora are built from trees that exist",
                corpora.len()
            ),
            repair: Repair::None,
        };
    }
    let mut message = Vec::new();
    if !gone.is_empty() {
        let listed: Vec<String> = gone
            .iter()
            .map(|(id, src)| format!("{id} ({})", src.as_deref().unwrap_or_default()))
            .collect();
        message.push(format!(
            "{} code corpus(es) built from a tree that no longer exists: {}; every \
             unscoped code question still answers from them",
            gone.len(),
            listed.join(", ")
        ));
    }
    if !unrecorded.is_empty() {
        message.push(format!(
            "{} code corpus(es) record no source tree: {}; `svrn tools call` cannot \
             scope to them from their repo (rebuild the chunk index with \
             `svrn code index <root> --corpus-id <id>`)",
            unrecorded.len(),
            unrecorded.join(", ")
        ));
    }
    let repair = if gone.is_empty() {
        Repair::None
    } else {
        Repair::Manual(format!(
            "remove each with `svrn corpus remove <id> --yes` ({}), then restart the \
             daemon: its merged graph holds a removed corpus until it reloads",
            gone.iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
    CheckResult {
        name,
        layer: Layer::Sovereign,
        status: CheckStatus::Warning,
        message: message.join(". "),
        repair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(id: &str, src: Option<&str>) -> (String, Option<String>) {
        (id.to_string(), src.map(str::to_string))
    }

    #[test]
    fn a_corpus_whose_tree_is_gone_is_named_with_its_removal() {
        let r = code_corpus_source_findings(
            &[
                corpus("svrngs", Some("/w/svrngs")),
                corpus("svrngs-plant", Some("/tmp/gone")),
            ],
            |p| p == "/w/svrngs",
        );
        assert!(matches!(r.status, CheckStatus::Warning), "{}", r.message);
        assert!(
            r.message.contains("svrngs-plant (/tmp/gone)"),
            "{}",
            r.message
        );
        assert!(!r.message.contains("svrngs (/w"), "{}", r.message);
        assert!(matches!(&r.repair, Repair::Manual(m) if m.contains("svrngs-plant")));
    }

    #[test]
    fn a_corpus_that_records_no_tree_is_named_apart() {
        let r = code_corpus_source_findings(&[corpus("commonwealth-ai", None)], |_| true);
        assert!(matches!(r.status, CheckStatus::Warning), "{}", r.message);
        assert!(
            r.message.contains("record no source tree: commonwealth-ai"),
            "{}",
            r.message
        );
    }

    #[test]
    fn every_tree_present_passes_and_none_indexed_skips() {
        let r = code_corpus_source_findings(&[corpus("a", Some("/w/a"))], |_| true);
        assert!(matches!(r.status, CheckStatus::Passed), "{}", r.message);
        let r = code_corpus_source_findings(&[], |_| true);
        assert!(matches!(r.status, CheckStatus::Skipped), "{}", r.message);
    }
}
