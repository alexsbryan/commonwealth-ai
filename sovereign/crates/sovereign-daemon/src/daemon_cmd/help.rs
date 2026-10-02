// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal twin of `sovereign-cli-shared/src/help.rs` — the daemon crate may
//! not depend on cli-shared (svrn-package; no reverse edge onto the host), so
//! the subset of the shared formatter this module's two HELP consts use is
//! reimplemented here verbatim. The twin is named, not silent (ARCH §18.3).

/// A rendered help block. All fields are `'static` because help text
/// lives as compile-time string literals on module-level `const`s.
pub struct Help {
    /// Full command name shown in the banner, e.g. `"svrn daemon vram-plan"`.
    pub command: &'static str,
    /// One-sentence imperative summary (no trailing period needed — we add it).
    pub summary: &'static str,
    /// Ordered list of sections. Order in the slice is render order.
    pub sections: &'static [HelpSection],
}

/// A named, formatted block inside a `Help` (used variants only).
pub enum HelpSection {
    /// Raw usage line(s). `"svrn daemon [--flag]"`.
    Usage(&'static str),
    /// A list of subcommands. Each entry is `(name, one-line description)`.
    Subcommands(&'static [(&'static str, &'static str)]),
    /// A list of flags. Each entry is `("--flag <v>", description)`.
    Flags(&'static [(&'static str, &'static str)]),
    /// Concrete examples users can copy-paste. Each is
    /// `(command_line, purpose)`.
    Examples(&'static [(&'static str, &'static str)]),
    /// Free-form paragraph for caveats, invariants, or links.
    Notes(&'static str),
}

/// Detect whether the user asked for help. Matches `--help`, `-h`, and
/// the bare word `help` anywhere in the argument list.
pub fn wants_help(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
}

/// Render a `Help` block to stderr (help output doesn't go to stdout
/// so users can still pipe stdout cleanly on error paths).
pub fn print(h: &Help) {
    eprintln!();
    eprintln!("  {}", h.command);
    eprintln!(
        "  {}",
        "─".repeat(h.command.chars().count() + 2).min_width(50)
    );
    eprintln!("  {}", h.summary);

    for section in h.sections {
        eprintln!();
        match section {
            HelpSection::Usage(line) => {
                eprintln!("  Usage:");
                for l in line.lines() {
                    eprintln!("    {l}");
                }
            }
            HelpSection::Subcommands(entries) => {
                eprintln!("  Subcommands:");
                print_table(entries);
            }
            HelpSection::Flags(entries) => {
                eprintln!("  Flags:");
                print_table(entries);
            }
            HelpSection::Examples(entries) => {
                eprintln!("  Examples:");
                for (cmd, purpose) in *entries {
                    eprintln!("    $ {cmd}");
                    eprintln!("        {purpose}");
                }
            }
            HelpSection::Notes(text) => {
                eprintln!("  Notes:");
                for line in text.lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
    eprintln!();
}

/// Two-column aligned printer for Subcommands / Flags sections. Pads
/// the left column to the longest entry so descriptions line up.
fn print_table(entries: &[(&str, &str)]) {
    let width = entries
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    for (name, desc) in entries {
        eprintln!("    {:width$}  {desc}", name, width = width);
    }
}

/// Utility extension so the banner underline doesn't collapse for
/// short command names. Keeps the help block visually balanced.
trait MinWidth {
    fn min_width(self, min: usize) -> String;
}

impl MinWidth for String {
    fn min_width(self, min: usize) -> String {
        if self.chars().count() >= min {
            self
        } else {
            let pad = min - self.chars().count();
            let mut out = self;
            for _ in 0..pad {
                out.push('─');
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_help_matches_all_forms() {
        assert!(wants_help(&["--help".into()]));
        assert!(wants_help(&["-h".into()]));
        assert!(wants_help(&["help".into()]));
        assert!(wants_help(&["run".into(), "--help".into()]));
    }

    #[test]
    fn wants_help_rejects_other_args() {
        assert!(!wants_help(&[]));
        assert!(!wants_help(&["--config".into(), "x.toml".into()]));
        assert!(!wants_help(&["helping".into()])); // not "help" exact
    }
}
