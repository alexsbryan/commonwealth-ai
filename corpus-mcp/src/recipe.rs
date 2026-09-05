// SPDX-License-Identifier: AGPL-3.0-or-later
//! `corpus recipe new` — the FIRST of `EPISTEMIC_INDEX.md` §4's three
//! commands, and the smallest of them.
//!
//! ## What was reused, and what was not moved (ARCH §19)
//!
//! The order asked for `svrn recipe new`'s scaffold "moved to where the
//! package can reach it". It is already there and nothing moved: the scaffold
//! is `corpus_engine::recipe_templates::{list_builtin_names, load_builtin,
//! instantiate}` — three functions in a crate this binary already links (the
//! single grandfathered `corpus-mcp → corpus-engine` row in
//! `quality/ARCH_LAYERS.toml`, and the crate that owns the templates because
//! it owns `Recipe`). `sovereign-cli-llm`'s `recipe_cmd/authoring.rs::cmd_new`
//! is a hand-rolled argv loop around those same three calls; it stays exactly
//! where it is, because `svrn` keeps its verbs (this order's Seams) and
//! because moving a CLI's argument parsing would not have moved the decider.
//!
//! So there is ONE scaffold implementation, in the lowest crate that has the
//! concept, with two callers (§10.6). What differs between the callers is the
//! SINK, not the scaffold: `svrn recipe new` prints to stdout unless `--out`
//! names a file; here `--id my-coins` writes `my-coins.toml`, because §4's
//! comment on that line is "writes my-coins.toml" and a person following three
//! commands should not have to redirect the first one into the second.
//! Neither ever overwrites.

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum RecipeCommand {
    /// Scaffold a recipe from a built-in ontology template.
    New(NewArgs),
}

#[derive(clap::Args, Debug)]
pub struct NewArgs {
    /// Ontology template to scaffold from. `--ontology list` names them all.
    #[arg(long)]
    pub ontology: String,

    /// Corpus id. Substituted into the template's `id` and `name`, and — with
    /// no `--out` — names the file written (`<id>.toml`).
    #[arg(long)]
    pub id: Option<String>,

    /// Write here instead of `<id>.toml`. `-` writes to stdout, which is what
    /// happens anyway when neither `--id` nor `--out` is given: with no id
    /// there is no name to give the file, and inventing one would put an
    /// un-asked-for `REPLACE_ME.toml` in the working directory.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

pub fn run(cmd: RecipeCommand) -> Result<()> {
    match cmd {
        RecipeCommand::New(args) => new(args),
    }
}

fn new(args: NewArgs) -> Result<()> {
    // `list` is a template name a person types, not a flag, because that is
    // the spelling `svrn recipe new --ontology list` already has and one
    // surface should not need two idioms for one question.
    if args.ontology == "list" {
        for name in corpus_engine::recipe_templates::list_builtin_names() {
            println!("{name}");
        }
        return Ok(());
    }
    // Unknown id is LOUD and names every template there is (ARCH §4) — the
    // error text is `load_builtin`'s own, so this host and `svrn` cannot
    // disagree about what exists.
    let template = corpus_engine::recipe_templates::load_builtin(&args.ontology)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let text = corpus_engine::recipe_templates::instantiate(template, args.id.as_deref());

    let sink = match (&args.out, &args.id) {
        (Some(p), _) if p.as_os_str() == "-" => None,
        (Some(p), _) => Some(p.clone()),
        (None, Some(id)) => Some(PathBuf::from(format!("{id}.toml"))),
        (None, None) => None,
    };
    let Some(path) = sink else {
        print!("{text}");
        return Ok(());
    };
    // Never overwrite. A scaffold that clobbers is a scaffold that eats the
    // types the person just spent an hour writing.
    if path.exists() {
        bail!(
            "{} exists; `recipe new` never overwrites — pass --out <path>, or a different --id",
            path.display()
        );
    }
    std::fs::write(&path, &text)?;
    println!("wrote {}", path.display());
    println!(
        "next: fill in the source path and the type guidance, then\n  \
         corpus-mcp ingest {}",
        path.display()
    );
    Ok(())
}
