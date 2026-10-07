// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path scoping on whole components — the one decider for "does this
//! repo-relative path lie under that one?", shared by
//! [`crate::converge::SourceScope`]'s include list and dry-report's `--scope`.

/// Does `path` lie under `prefix` on whole path components? `crates/svrngs`
/// takes in `crates/svrngs/src/a.rs` (and a file at exactly that path), never
/// `crates/svrngs-core/…`, which a raw `starts_with` did. A trailing `/` is
/// dropped first, so `crates/svrngs/` is the same prefix; an empty one takes
/// in every path.
pub fn under(prefix: &str, path: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    match path.strip_prefix(prefix) {
        _ if prefix.is_empty() => true,
        Some(rest) if rest.is_empty() || rest.starts_with('/') => true,
        Some(_) => {
            // Per path, so trace: a debug event here fires once per file of
            // every sibling directory (principle 1, per-item detail).
            tracing::trace!(prefix, path, "path_scope: string-prefix sibling refused");
            false
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::converge::SourceScope;

    fn including(prefix: &str) -> SourceScope {
        SourceScope {
            include_prefixes: vec![prefix.to_string()],
            ..SourceScope::default()
        }
    }

    /// `converge --include crates/svrngs` also took in `crates/svrngs-core`:
    /// the include list matched a raw string prefix — the defect dry-report's
    /// `--scope` had as I3 (svrngs audit, 2026-10-04).
    #[test]
    fn include_prefixes_match_whole_path_components() {
        for spelling in ["crates/svrngs", "crates/svrngs/"] {
            let s = including(spelling);
            assert!(
                !s.admits("crates/svrngs-core/src/lib.rs"),
                "`{spelling}` took in a sibling crate"
            );
            assert!(
                !s.admits("crates/svrngsx.rs"),
                "`{spelling}` took in a sibling file"
            );
            assert!(
                s.admits("crates/svrngs/src/lib.rs"),
                "`{spelling}` lost its own crate"
            );
        }
        let file = including("crates/svrngs/src/lib.rs");
        assert!(
            file.admits("crates/svrngs/src/lib.rs"),
            "a file scope admits that file"
        );
        assert!(
            !including("crates/svrngs").admits("crates/svrngs/tests/e2e.rs"),
            "the excluded segments still apply under an include"
        );
        assert!(
            including("").admits("anything/at/all.rs"),
            "an empty include takes in all"
        );
    }
}
