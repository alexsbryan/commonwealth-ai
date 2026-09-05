// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn quality map` — the quality surface, rendered from the registry.
//!
//! `svrn quality check` RUNS lanes; `svrn posture` reads artifacts other
//! commands wrote; this one renders `quality/instruments.toml` and runs
//! nothing at all. It is the replacement for four hand-maintained tables in
//! `sovereign-desktop/QUALITY_SURFACE.md` — layers, fidelity, load-bearing
//! flags, and what-runs-where — which is why it emits Markdown rather than a
//! terminal table: the output is meant to be read, pasted, and diffed against
//! `quality/quality-map.golden.md`.
//!
//! WHY A COMMAND AND NOT A GENERATED DOC. Both, in fact: the golden IS the
//! generated doc, refreshed with `--update-golden` and gated by a test in
//! `xtask/tests/`. The command exists because the question "what verifies
//! this, and where does it run" is asked mid-session, and opening a 295-line
//! doc to answer it is how the doc came to be maintained by hand in the first
//! place.
//!
//! The TABLES live in `kernel_types::quality::render`, beside the parser, for
//! the reason `render_rows` lives beside `Judgement`: a table is a claim about
//! the data next to it, and a claim rendered in two crates drifts. This module
//! is the CLI surface — flags, section selection, refusals.

use std::path::PathBuf;

use kernel_types::quality::{self, Registry};

const REGISTRY_REL: &str = "quality/instruments.toml";
const GOLDEN_REL: &str = "quality/quality-map.golden.md";

pub fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }

    // Repo-scoped by construction: the registry is repo data, so outside a
    // checkout this verb REFUSES rather than rendering an empty map (ARCH
    // §18.3 — a could-not-judge is not a pass).
    let Some(root) = crate::posture_cmd::find_repo_root() else {
        eprintln!(
            "svrn quality map: not inside a source checkout — this verb renders \
             {REGISTRY_REL}, which only a checkout has."
        );
        return 3;
    };
    let path = root.join(REGISTRY_REL);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("svrn quality map: cannot read {}: {e}", path.display());
            return 3;
        }
    };
    let registry = match Registry::parse(&text) {
        Ok(r) => r,
        Err(errs) => {
            eprintln!("svrn quality map: {REGISTRY_REL} is not valid:");
            for e in &errs {
                eprintln!("  ✗ {e}");
            }
            return 1;
        }
    };

    let section = args.iter().find_map(|a| Section::parse(a));
    let unknown = args
        .iter()
        .find(|a| a.starts_with("--") && Section::parse(a).is_none() && *a != "--update-golden");
    if let Some(flag) = unknown {
        // Never silently widen to the whole map: an unrecognised flag is
        // refused with the legal ones named, the same contract
        // `quality check --lane` already keeps for an unknown lane id.
        eprintln!(
            "svrn quality map: unknown flag `{flag}` — sections are {}",
            Section::words()
        );
        return 2;
    }

    let rendered = match section {
        Some(Section::Layers) => quality::render_layers(&registry),
        Some(Section::Fidelity) => quality::render_fidelity(&registry),
        Some(Section::LoadBearing) => quality::render_load_bearing(&registry),
        Some(Section::Where) => quality::render_where(&registry),
        None => quality::render_map(&registry),
    };

    if args.iter().any(|a| a == "--update-golden") {
        if section.is_some() {
            eprintln!(
                "svrn quality map: --update-golden writes the WHOLE map; drop the section flag."
            );
            return 2;
        }
        return write_golden(&root.join(GOLDEN_REL), &rendered);
    }

    print!("{rendered}");
    0
}

fn write_golden(path: &PathBuf, rendered: &str) -> i32 {
    match std::fs::write(path, rendered) {
        Ok(()) => {
            eprintln!("wrote {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("svrn quality map: cannot write {}: {e}", path.display());
            1
        }
    }
}

/// Which table. A closed set with its words named in the refusal, so a typo
/// is a two-second fix rather than a whole map to scroll.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Layers,
    Fidelity,
    LoadBearing,
    Where,
}

impl Section {
    /// The variants, once. `parse` and the refusal text both derive from this
    /// array, so a fifth section cannot be added to one and forgotten in the
    /// other — which is the only way either could go wrong.
    const ALL: [Section; 4] = [
        Section::Layers,
        Section::Fidelity,
        Section::LoadBearing,
        Section::Where,
    ];

    fn flag(self) -> &'static str {
        match self {
            Section::Layers => "--layers",
            Section::Fidelity => "--fidelity",
            Section::LoadBearing => "--load-bearing",
            Section::Where => "--where",
        }
    }

    fn parse(arg: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|s| s.flag() == arg)
    }

    /// What a refusal names. Derived, so it cannot fall behind the set it
    /// describes — a refusal that lists three of four sections is worse than
    /// no refusal, because it reads authoritative.
    fn words() -> String {
        Section::ALL
            .iter()
            .map(|s| s.flag())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "svrn quality map",
    summary: "Render quality/instruments.toml: every instrument, its fidelity, what silently \
              weakens it, and where it runs. Reads only; runs nothing.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "svrn quality map [--layers|--fidelity|--load-bearing|--where] [--update-golden]",
        ),
        crate::util::help::HelpSection::Examples(&[
            ("svrn quality map", "all four tables, as Markdown"),
            (
                "svrn quality map --where",
                "which venue runs what — plus what CI does not run, and what nothing runs",
            ),
            (
                "svrn quality map --load-bearing",
                "flags and preconditions whose absence leaves a green meaning less",
            ),
            (
                "svrn quality map --update-golden",
                "refresh quality/quality-map.golden.md after a registry edit",
            ),
        ]),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_flag_parses_and_nothing_else_does() {
        for (arg, want) in [
            ("--layers", Section::Layers),
            ("--fidelity", Section::Fidelity),
            ("--load-bearing", Section::LoadBearing),
            ("--where", Section::Where),
        ] {
            assert!(Section::parse(arg) == Some(want), "{arg}");
        }
        assert!(Section::parse("--loadbearing").is_none());
        assert!(Section::parse("--all").is_none());
    }

    /// The refusal has to name the legal words, or a typo costs a doc read.
    #[test]
    fn the_section_words_are_named_in_one_place() {
        for word in ["--layers", "--fidelity", "--load-bearing", "--where"] {
            assert!(
                Section::WORDS.contains(word),
                "{word} missing from the refusal text"
            );
        }
    }
}
