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
