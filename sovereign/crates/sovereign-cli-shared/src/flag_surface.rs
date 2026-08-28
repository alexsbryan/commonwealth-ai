// SPDX-License-Identifier: AGPL-3.0-or-later
//! One rendering rule for every `#[derive(clap::Parser)]` flag surface in the
//! `sovereign-cli-*` binaries.
//!
//! Lives HERE rather than in any one CLI crate because more than one of them
//! now derives its flag surface: `sovereign-cli-llm` (three commands) and
//! `sovereign-cli` (the dispatcher's own globals, `notes retrieval-audit`).
//! A per-crate copy would be two implementations of one rendering rule — the
//! §10.6 failure the `hot-path-reuse` campaign exists to remove — so the seam
//! moved up to the crate every sibling already links (2026-08-23).
//!
//! ## Why this is a function and not three `map_err` closures
//!
//! `clap::Error::to_string()` already begins `error: `, and every caller here
//! prefixes its own (`error: ` in `bench_cmd`, `router fit: ` in
//! `router_fit_cmd`, `retrieval-audit: ` in `notes_retrieval_cmd`) because the
//! hand-kept rules those parsers still enforce —
//! `--save-baseline needs a full run …`, `need --cold or --warm …` — arrive
//! with no prefix at all and need one. Composing the two produced
//! `error: error: the argument '--prod-pipeline' cannot be used with '--synth'`
//! in all three converted commands.
//!
//! The prefix has exactly one owner: the CALLER. [`parse`] is what makes that
//! true, by handing back a bare message whatever its origin. Doing it in three
//! `map_err` closures instead would be three implementations of one rendering
//! rule (ARCH §10.6), and the three are the exemplars every future conversion
//! will be copied from — a defect here is inherited, not isolated.
//!
//! ## Why a plain `strip_prefix` is safe
//!
//! Measured, not assumed. The `color` feature IS on (`clap/default`, v4.6.1),
//! so the worry is real: a styled prefix would make the strip match nothing.
//! It does not apply, because clap renders colour in `Error::print()` — which
//! writes to a stream — and NOT in `Display`. `to_string()` therefore carries a
//! literal `error: ` even for a surface built with `ColorChoice::Always`.
//!
//! That is an upstream property this module depends on and does not control,
//! so it is pinned by a test rather than trusted (ARCH §18.4). A first attempt
//! at this module hard-set `ColorChoice::Never` and called it load-bearing;
//! deleting the line changed no test, which is how the claim was found to be
//! wrong. The pin is gone and the fact it was guessing at is measured.

/// Parse a flag surface, returning a message a caller can prefix.
///
/// The counterpart of `#[command(name = …)]` on the surface itself: `name`
/// makes the `Usage:` line inside this message say what the user typed rather
/// than `sovereign-cli-llm`, since these commands are reached through a
/// dispatcher and never invoked under the binary's own name.
pub fn parse<T: clap::Parser>(args: &[String]) -> Result<T, String> {
    T::try_parse_from(args).map_err(bare)
}

/// Strip the prefix `clap` renders so the caller's is the only one.
fn bare(e: clap::Error) -> String {
    let s = e.to_string();
    s.strip_prefix("error: ")
        .unwrap_or(&s)
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    /// A surface asking for colour as loudly as `clap` allows. The three real
    /// ones inherit `ColorChoice::Auto`, which would style whenever stderr is a
    /// terminal — a condition no test can reproduce. This asks for the styling
    /// unconditionally instead, so [`super::bare`] is checked against the
    /// hardest case rather than the one `cargo test` happens to produce.
    #[derive(clap::Parser, Debug)]
    #[command(no_binary_name = true, color = clap::ColorChoice::Always)]
    struct Coloured {
        #[arg(long)]
        thing: Option<String>,
    }

    #[test]
    fn the_prefix_is_stripped_even_when_the_surface_asks_for_colour() {
        let err = super::parse::<Coloured>(&["--nope".to_string()]).unwrap_err();
        assert!(
            !err.contains('\u{1b}'),
            "clap started styling Display, not just print(); `bare` must then \
             build the command with .color(ColorChoice::Never) instead. got: {err:?}"
        );
        assert!(!err.starts_with("error:"), "got: {err}");
    }
}
