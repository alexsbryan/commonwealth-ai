// SPDX-License-Identifier: AGPL-3.0-or-later
//! `warm-cache` and `fetch-model` are serve's (phase-b-22): cmnwlth's CLI
//! names neither model transfer nor the tensor-cache warm-up. The remaining
//! sovereign_inference uses are plan/bench's, which pb-serve-placement moves.

use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).expect("read source");
            out.push((path.display().to_string(), text));
        }
    }
}

#[test]
fn cli_mesh_names_no_serve_weight_verb_surface() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty(), "no sources found under src/");
    // Spelled in pieces so this file never matches itself.
    let forbidden = [
        concat!("model", "_fetch"),
        concat!("warm_cache", "_from_gguf"),
        concat!("embedded::", "default_cache_dir"),
    ];
    let hits: Vec<String> = files
        .iter()
        .flat_map(|(path, text)| {
            forbidden
                .iter()
                .filter(move |f| text.contains(**f))
                .map(move |f| format!("{path}: {f}"))
        })
        .collect();
    assert!(
        hits.is_empty(),
        "serve's weight verbs are back in cli-mesh: {hits:?}"
    );
}
