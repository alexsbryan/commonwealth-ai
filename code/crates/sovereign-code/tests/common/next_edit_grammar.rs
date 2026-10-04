// SPDX-License-Identifier: AGPL-3.0-or-later
//! Test-only grammar lookup for the editor door's e2e and offline scorer
//! (pb-meshapp-rest). The real registry is corpus-engine's, supplied by the
//! stock binary; code may not name it (dev edges included), so these dev
//! targets register the grammars the lanes judge — rust, typescript (with the
//! `.tsx` split, a DIFFERENT grammar from `.ts`), go, and python — under the
//! registry's own ids, as code-next-edit's own tests do.

use sovereign_code::face::Grammar;

pub fn grammar_for(ext: &str) -> Option<Grammar> {
    let grammar = |id, language| Some(Grammar { id, language });
    match ext {
        "rs" => grammar("rust", tree_sitter_rust::LANGUAGE.into()),
        "ts" => grammar(
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        "tsx" => grammar("typescript", tree_sitter_typescript::LANGUAGE_TSX.into()),
        "go" => grammar("go", tree_sitter_go::LANGUAGE.into()),
        "py" | "pyi" => grammar("python", tree_sitter_python::LANGUAGE.into()),
        _ => None,
    }
}
