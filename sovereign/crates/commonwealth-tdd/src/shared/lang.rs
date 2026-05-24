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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_extensions_per_language() {
        assert_eq!(Language::Python.source_extensions(), &[".py"]);
        assert_eq!(Language::Rust.source_extensions(), &[".rs"]);
        assert_eq!(Language::Go.source_extensions(), &[".go"]);
        let ts = Language::TypeScript.source_extensions();
        assert!(ts.contains(&".ts") && ts.contains(&".tsx"));
    }
}
