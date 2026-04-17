//! Status line printing with the same ✓ / ✗ / ⚠ symbols every
//! subcommand uses in its output. Before this, each file hand-rolled
//! `eprintln!("    ✓ {msg}")` — that was fine in isolation but made
//! output drift (some call sites used 2-space indent, some 4-space;
//! some wrote to stdout, some to stderr).
//!
//! Convention: all status lines go to **stderr** so users can pipe
//! the informational half of a command without losing its progress
//! narration. Data output (config blobs, URLs, json) goes to stdout.

use std::fmt::Display;

/// The three classes of status line a subcommand produces. Keep the
/// set minimal — if you need another, consider whether a plain
/// `eprintln!` conveys the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Green ✓ — a step succeeded.
    Pass,
    /// Red ✗ — a step failed (usually followed by `return 1`).
    Fail,
    /// Yellow ⚠ — a step succeeded with caveats, or was skipped.
    Warn,
}

impl Status {
    /// Single-character unicode glyph. Bare so callers can compose it
    /// into custom layouts when a full `line()` doesn't fit.
    pub fn symbol(self) -> &'static str {
        match self {
            Status::Pass => "\u{2713}", // ✓
            Status::Fail => "\u{2717}", // ✗
            Status::Warn => "\u{26a0}", // ⚠
        }
    }

    /// Print a single line with the status symbol prefixed by
    /// `indent` spaces. Example:
    ///
    ///     Status::Pass.line(4, ".sovereign/SOVEREIGN.md")
    ///     // stderr: "    ✓ .sovereign/SOVEREIGN.md"
    pub fn line(self, indent: usize, msg: impl Display) {
        eprintln!("{:indent$}{} {msg}", "", self.symbol(), indent = indent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_match_prior_codepoints() {
        // Locks in the exact codepoints that were hand-embedded across
        // the old call sites — protects against someone "normalising"
        // to an emoji or ASCII fallback later.
        assert_eq!(Status::Pass.symbol(), "\u{2713}");
        assert_eq!(Status::Fail.symbol(), "\u{2717}");
        assert_eq!(Status::Warn.symbol(), "\u{26a0}");
    }

    #[test]
    fn pass_fail_warn_are_distinct() {
        assert_ne!(Status::Pass.symbol(), Status::Fail.symbol());
        assert_ne!(Status::Pass.symbol(), Status::Warn.symbol());
        assert_ne!(Status::Fail.symbol(), Status::Warn.symbol());
    }
}
