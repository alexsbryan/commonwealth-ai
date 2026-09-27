// SPDX-License-Identifier: AGPL-3.0-or-later
//! boundary-gate's `[[distribution]]` half (phase-b-29 Q4): a composition root
//! outside every package may name its own crates, the shared leaves and its
//! declared faces, and of each face only the declared items.
//!
//! Its own module because boundary_gate.rs sits in the 800-1200 band. The
//! direct-edge rule is the shared parser's (`arch_layers::evaluate_distributions`);
//! this half adds what an edge cannot express: the filesystem rules every
//! governed crate answers to, the face-ITEM scan over the distribution's
//! source, and the fixed `max_code_lines` cap.

use std::collections::BTreeMap;
use std::path::Path;

use crate::boundary_gate::{include_escapes, rs_files, runtime_root_escapes};

/// Run every distribution rule, pushing failures into `fails` (one list, one
/// count, as the package rules). Returns the distribution crates checked.
pub(crate) fn check(
    root: &Path,
    map: &arch_layers::LayerMap,
    edges: &[arch_layers::DepEdge],
    dir_of: &BTreeMap<&str, &str>,
    fails: &mut Vec<String>,
) -> usize {
    fails.extend(
        arch_layers::evaluate_distributions(map, edges)
            .iter()
            .map(|v| v.describe()),
    );
    let mut checked = 0;
    for dist in &map.distributions {
        let scope = dist.name.as_str();
        let mut code_lines = 0usize;
        for name in &dist.crates {
            let Some(rel) = dir_of.get(name.as_str()) else {
                continue;
            };
            checked += 1;
            let dir = root.join(rel);
            if dir.join("build.rs").exists() {
                fails.push(format!(
                    "[{scope}] {name}: has a build.rs — a composition root composes \
                     programs and carries no build script"
                ));
            }
            include_escapes(&dir, name, scope, fails);
            runtime_root_escapes(&dir, name, scope, fails);
            face_items(&dir, name, dist, fails);
            match crate::size_gate::crate_code_lines(root, &dir, name) {
                Ok(n) => code_lines += n,
                Err(e) => fails.push(format!(
                    "[{scope}] {name}: size cap could not be measured: {e}"
                )),
            }
        }
        if let Some(f) = over_cap(dist, code_lines) {
            fails.push(f);
        }
    }
    checked
}

/// The fixed cap: set per row, never ratcheted (phase-b-30 Group 2).
fn over_cap(dist: &arch_layers::Distribution, code_lines: usize) -> Option<String> {
    (code_lines > dist.max_code_lines).then(|| {
        format!(
            "[{}] {code_lines} code lines over the distribution's fixed cap of {} — a \
             composition root that grows is doing a program's work: move it into the \
             program and name it on the face; the cap is never re-pinned",
            dist.name, dist.max_code_lines
        )
    })
}

/// Rule: of each face crate, the distribution's `src/` names only the face's
/// items. Scans `src/` alone: the crate's tests drive the built binary.
fn face_items(dir: &Path, name: &str, dist: &arch_layers::Distribution, fails: &mut Vec<String>) {
    let mut files = Vec::new();
    rs_files(&dir.join("src"), &mut files);
    files.sort();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (line, face, used) in scan_face_items(&text, dist) {
            fails.push(format!(
                "[{}] {name}: {rel}:{line} names `{}::{used}`, which is not on the `{}` \
                 face (items: {}) — spell every face item by its full path, and put \
                 what the distribution needs on the program's face",
                dist.name,
                face.krate.replace('-', "_"),
                face.package,
                face.items.join(", ")
            ));
        }
    }
}

/// Every `<face_krate>::<path>` in `text` (comments stripped) whose path is
/// not a face item or below one, as `(line, face, path)`. A bare
/// `use krate::{…}` names the empty path and is reported: a grouped import
/// would hide which items the distribution names.
fn scan_face_items<'a>(
    text: &str,
    dist: &'a arch_layers::Distribution,
) -> Vec<(usize, &'a arch_layers::Face, String)> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or("");
        for face in &dist.faces {
            let ident = face.krate.replace('-', "_");
            let needle = format!("{ident}::");
            let mut from = 0;
            while let Some(at) = line[from..].find(&needle) {
                let start = from + at;
                from = start + needle.len();
                let before = line[..start].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let path = path_at(&line[from..]);
                let allowed = face
                    .items
                    .iter()
                    .any(|it| path == *it || path.starts_with(&format!("{it}::")));
                if !allowed {
                    out.push((i + 1, face, path));
                }
            }
        }
    }
    out
}

/// The `a::b::c` path at the start of `s`, stopping at the first character
/// that is not part of one.
fn path_at(s: &str) -> String {
    let mut end = 0;
    let b = s.as_bytes();
    loop {
        let seg = b[end..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
            .count();
        if seg == 0 {
            break;
        }
        end += seg;
        if s[end..].starts_with("::")
            && s[end + 2..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            end += 2;
        } else {
            break;
        }
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock() -> arch_layers::Distribution {
        arch_layers::Distribution {
            name: "stock".into(),
            crates: vec!["sovereign-stock".into()],
            faces: vec![arch_layers::Face {
                package: "serve".into(),
                krate: "sovereign-serve".into(),
                items: vec!["bundles".into(), "assemble".into()],
            }],
            doc: "docs/FIVE_PROGRAMS.md".into(),
            max_code_lines: 300,
        }
    }

    /// The row's gate test: a non-face `sovereign_serve::` item is named, a
    /// face item (and a path below it) is not, and comments do not count.
    #[test]
    fn a_non_face_item_is_named() {
        let d = stock();
        let text = "fn main() {\n\
                    let r = sovereign_serve::bundles(p);\n\
                    let a = sovereign_serve::assemble::Parts::default();\n\
                    // sovereign_serve::run is only mentioned here\n\
                    let x = sovereign_serve::run(&args);\n\
                    my_sovereign_serve::run();\n\
                    }\n";
        let v = scan_face_items(text, &d);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!((v[0].0, v[0].2.as_str()), (5, "run"));
        // A grouped import hides what it names, so it is named itself.
        let v = scan_face_items("use sovereign_serve::{bundles, run};", &d);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].2, "");
    }

    /// The row's gate test: a distribution over its fixed cap is named.
    #[test]
    fn a_distribution_over_its_cap_is_named() {
        let d = stock();
        assert!(over_cap(&d, 300).is_none());
        let f = over_cap(&d, 301).expect("301 > 300 is named");
        assert!(f.starts_with("[stock] 301 code lines over"), "{f}");
    }
}
