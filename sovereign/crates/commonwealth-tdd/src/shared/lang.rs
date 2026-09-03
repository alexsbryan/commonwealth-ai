// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-language tag — dispatches test-output parsing.
//!
//! Renamed from bench's `WitnessLanguage` because outside of bench's
//! witness vocabulary this is just "what language is the source".
//! The bench's `WitnessLanguage` lives on; conversion is one match
//! arm in the bench adapter.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Language {
    Rust,
    Go,
    TypeScript,
    Python,
}

impl Language {
    pub fn id(&self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Go => "Go",
            Language::TypeScript => "TypeScript",
            Language::Python => "Python",
        }
    }

    /// File extensions the structural-test templates walk when
    /// they count source files. Used by `tasks::structural::*` so
    /// the Rust template walks `.rs`, the Python template walks
    /// `.py`, etc. — without each template having to re-encode
    /// the per-language extension list.
    pub fn source_extensions(&self) -> &'static [&'static str] {
        match self {
            Language::Python => &[".py"],
            Language::Rust => &[".rs"],
            Language::TypeScript => &[".ts", ".tsx", ".js", ".jsx"],
            Language::Go => &[".go"],
        }
    }

    /// Infer the language from a file's extension. Returns
    /// `Language::Python` for unrecognized extensions because pytest
    /// is the most common case the test framework auto-detector
    /// falls back to; a wrong guess still lets the loop run a test
    /// and fail loudly at parse time.
    pub fn from_path(p: &str) -> Self {
        if p.ends_with(".py") {
            Language::Python
        } else if p.ends_with(".rs") {
            Language::Rust
        } else if p.ends_with(".go") {
            Language::Go
        } else if p.ends_with(".ts") || p.ends_with(".tsx") || p.ends_with(".js") {
            Language::TypeScript
        } else {
            Language::Python
        }
    }

    /// Infer the language from the verify command's own runner name —
    /// the strongest signal a trial has, and the one that cannot be
    /// fooled by workdir shape. [`super::source::discover_source_files`]
    /// picks the SHALLOWEST source file (and walks only 3 levels down),
    /// so on a monorepo root a stray `scripts/*.py` outranks every
    /// workspace member's `.rs` and the run gets parsed with the wrong
    /// grammar: measured 2026-09-02, six solve attempts against a cargo
    /// workspace all returned `no_baseline` for exactly that reason.
    /// Command first, file discovery second, default last — see
    /// `trial.rs`'s baseline setup.
    pub fn from_verify_cmd(cmd: &str) -> Option<Self> {
        let c = cmd.to_ascii_lowercase();
        if c.contains("cargo ") || c.contains("cargo-") {
            Some(Self::Rust)
        } else if c.contains("go test") || c.contains("go vet") {
            Some(Self::Go)
        } else if c.contains("vitest")
            || c.contains("jest")
            || c.contains("playwright")
            || c.contains("node --test")
            || c.contains("npm ")
            || c.contains("pnpm ")
            || c.contains("yarn ")
            || c.contains("npx ")
            || c.contains("tsc ")
        {
            Some(Self::TypeScript)
        } else if c.contains("pytest") || c.contains("python") || c.contains("uv run") {
            Some(Self::Python)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

        /// The regression this exists for: a cargo command must never be
    /// parsed as Python just because the workdir's shallowest source
    /// file is one. Table names the runner, not the repo shape.
    #[test]
    fn the_verify_command_names_the_parser() {
        assert_eq!(
            Language::from_verify_cmd("CARGO_TARGET_DIR=/t cargo test -p x --lib"),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_verify_cmd("cargo nextest run"),
            Some(Language::Rust)
        );
        assert_eq!(Language::from_verify_cmd("go test ./..."), Some(Language::Go));
        assert_eq!(
            Language::from_verify_cmd("npx vitest run"),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_verify_cmd("pytest -q"),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_verify_cmd("python -m pytest tests/"),
            Some(Language::Python)
        );
        // Nothing a parser could be picked from — the caller falls
        // through to file discovery, not to a guess from this name.
        assert_eq!(Language::from_verify_cmd("echo hello"), None);
        assert_eq!(Language::from_verify_cmd(""), None);
    }

    #[test]
    fn source_extensions_per_language() {
        assert_eq!(Language::Python.source_extensions(), &[".py"]);
        assert_eq!(Language::Rust.source_extensions(), &[".rs"]);
        assert_eq!(Language::Go.source_extensions(), &[".go"]);
        let ts = Language::TypeScript.source_extensions();
        assert!(ts.contains(&".ts") && ts.contains(&".tsx"));
    }
}
