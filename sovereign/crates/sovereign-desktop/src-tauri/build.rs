fn main() {
    // The corpus catalog used to be loaded from a build-time
    // `data/corpora.toml` bundled via `include_str!`, but that file was
    // workspace-gitignored and didn't survive a fresh checkout. The
    // catalog now lives entirely in `corpus_engine::recipe::builtin_recipes()`
    // (Rust source), so there's nothing to bundle here.
    tauri_build::build();
}
