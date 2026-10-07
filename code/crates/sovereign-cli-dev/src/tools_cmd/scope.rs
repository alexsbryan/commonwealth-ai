// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus a `tools call` is about, from where it is run: the code corpus
//! whose recorded `source_path` is the cwd's git root. The CLI's half of
//! `ToolContext::corpus_scope`; MCP's is the `x-svrn-corpus` header.

use std::path::{Path, PathBuf};

/// The one code corpus in `corpora` built from `root`, matched on its
/// recorded `source_path` and never on its name. `Err` is the line that says
/// why the call searches every corpus instead: no repo, no match, or more
/// than one.
pub(super) fn scope_for_root(
    corpora: &[(String, Option<PathBuf>)],
    root: Option<&Path>,
) -> Result<String, String> {
    let Some(root) = root else {
        return Err(
            "corpus scope: every code corpus (the working directory is not in a git repo)"
                .to_string(),
        );
    };
    let built: Vec<&str> = corpora
        .iter()
        .filter(|(_, src)| src.as_deref() == Some(root))
        .map(|(id, _)| id.as_str())
        .collect();
    match built.as_slice() {
        [id] => Ok(id.to_string()),
        [] => Err(format!(
            "corpus scope: every code corpus (none is built from {})",
            root.display()
        )),
        many => Err(format!(
            "corpus scope: every code corpus ({} are built from {}: {})",
            many.len(),
            root.display(),
            many.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpora() -> Vec<(String, Option<PathBuf>)> {
        vec![
            ("a".into(), Some(PathBuf::from("/w/b"))),
            ("b".into(), Some(PathBuf::from("/w/a"))),
            ("c".into(), None),
        ]
    }

    /// FAILING INPUT: a resolver keyed on the directory's name. Run from
    /// `/w/b`, it would answer `b`; the corpus built from `/w/b` is `a`.
    #[test]
    fn the_corpus_built_from_the_repo_is_the_scope_whatever_the_names() {
        assert_eq!(
            scope_for_root(&corpora(), Some(Path::new("/w/b"))),
            Ok("a".to_string())
        );
    }

    #[test]
    fn a_repo_no_corpus_was_built_from_searches_every_corpus_and_says_so() {
        let why = scope_for_root(&corpora(), Some(Path::new("/tmp"))).unwrap_err();
        assert!(
            why.contains("every code corpus") && why.contains("/tmp"),
            "{why}"
        );
        let why = scope_for_root(&corpora(), None).unwrap_err();
        assert!(why.contains("not in a git repo"), "{why}");
    }

    #[test]
    fn two_corpora_built_from_one_repo_are_named_not_picked_between() {
        let mut twice = corpora();
        twice.push(("d".into(), Some(PathBuf::from("/w/b"))));
        let why = scope_for_root(&twice, Some(Path::new("/w/b"))).unwrap_err();
        assert!(why.contains("a, d"), "{why}");
    }
}
