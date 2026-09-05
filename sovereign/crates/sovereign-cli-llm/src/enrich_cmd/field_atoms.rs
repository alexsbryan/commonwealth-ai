// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich field-atoms <corpus> [--into <corpus>]` — project a corpus's
//! existing `field_skeleton.json` into `Question` and `Position` atoms in an
//! atlas (ei-7b).
//!
//! The sibling of [`summary_atoms`](super::summary_atoms) and deliberately
//! shaped like it: one positional corpus id, the same `data_dir` derivation, no
//! daemon and no inference. **It extracts nothing** — the 549 SEP canonical
//! questions were extracted in April 2026 and already sit in the index; this
//! moves them into the atlas so the ambient "Field guide" digest can be
//! rendered from the same store the rest of retrieval reads.
//!
//! Every mechanism it uses is somebody else's and is reached, not
//! reimplemented (ARCH §19):
//!
//! | What | Whose |
//! |---|---|
//! | reading the v1 artifact | `CorpusIndex::load_field_skeleton` |
//! | skeleton → atoms | `field_atoms::skeleton_to_atoms` |
//! | writing them, idempotently | `field_atoms::publish_to_atlas` |
//! | the digest it prints | `FieldSkeleton::render_landscape` — the same renderer `turn_prepass` calls |
//!
//! `--into` writes the atoms into a DIFFERENT corpus's atlas. That is how a
//! fixture copy is built without writing into a control corpus, and it is the
//! only way this command touches an atlas other than the source's.

use corpus_engine::enrichment::field_atoms::{publish_to_atlas, skeleton_from_atoms};
use corpus_engine::CorpusIndex;
use sovereign_cli_shared::help;

/// The budget `turn_prepass::splice_ambient_field_digests` renders at. Repeated
/// here only so `--show-digest` prints what a turn would actually see; the
/// renderer and its section shape are not duplicated.
const FIELD_DIGEST_BUDGET_TOKENS: usize = 250;

pub async fn cmd_field_atoms(args: &[String]) -> i32 {
    if help::wants_help(args) {
        print_usage();
        return 0;
    }
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let into = flag_value(args, "--into");
    let show_digest = args.iter().any(|a| a == "--show-digest");
    let corpus_id = match positional.first() {
        Some(c) => (*c).clone(),
        None => {
            eprintln!("error: missing <corpus>\n");
            print_usage();
            return 2;
        }
    };
    let target_id = into.unwrap_or_else(|| corpus_id.clone());

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root());
    let indexes_dir = data_dir.join("indexes");
    for id in [&corpus_id, &target_id] {
        if !indexes_dir.join(id).exists() {
            eprintln!(
                "error: corpus '{id}' is not installed at {}",
                indexes_dir.join(id).display()
            );
            return 1;
        }
    }

    let index = match CorpusIndex::open(&indexes_dir.join(&corpus_id)).await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: could not open '{corpus_id}': {e}");
            return 1;
        }
    };
    // The absence is REPORTED, never rendered as an empty success (§18.3): a
    // corpus with no legacy skeleton has nothing to migrate, and saying "0
    // atoms written" would read like a corpus whose field model was empty.
    let skeleton = match index.load_field_skeleton() {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!(
                "error: '{corpus_id}' has no field_skeleton.json — nothing to migrate. \
                 (An `AtlasAtoms` domain does not write that file since ei-7b; a corpus \
                 enriched after the port already has its field model in the atlas.)"
            );
            return 1;
        }
        Err(e) => {
            eprintln!("error: could not read '{corpus_id}' field skeleton: {e}");
            return 1;
        }
    };

    let heading_before = format!("Field guide — {corpus_id}");
    let digest_before = skeleton.render_landscape(&heading_before, FIELD_DIGEST_BUDGET_TOKENS);
    println!(
        "Projecting '{corpus_id}' field skeleton ({} canonical questions, {} open) \
         into '{target_id}' atlas…",
        skeleton.canonical_questions.len(),
        skeleton.open_questions.len(),
    );

    let atlas_dir = indexes_dir
        .join(&target_id)
        .join(corpus_engine::enrichment::atlas::ATLAS_DIRNAME);
    let report = match publish_to_atlas(&atlas_dir, &skeleton) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: could not publish to {}: {e}", atlas_dir.display());
            return 1;
        }
    };
    println!("  {}", report.describe());

    // Read the atoms BACK and render the digest from them — the only honest
    // confirmation that the move preserved what a turn will see (§18.4:
    // validate the instrument, not just the write).
    let atoms = match corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir) {
        Ok(f) => f.atoms,
        Err(e) => {
            eprintln!("error: wrote the atoms but could not read them back: {e}");
            return 1;
        }
    };
    let heading_after = format!("Field guide — {target_id}");
    let digest_after = skeleton_from_atoms(&target_id, &atoms)
        .render_landscape(&heading_after, FIELD_DIGEST_BUDGET_TOKENS);

    // Compare on the BODY, not the heading — `--into` deliberately renames the
    // corpus, and a heading difference is not a content difference.
    let body_before = digest_before.trim_start_matches(heading_before.as_str());
    let body_after = digest_after.trim_start_matches(heading_after.as_str());
    if body_before == body_after {
        println!(
            "  digest: identical to the v1 file's ({} chars)",
            body_after.len()
        );
    } else {
        println!(
            "  digest: CHANGED — v1 rendered {} chars, the atoms render {}",
            body_before.len(),
            body_after.len()
        );
    }
    if show_digest {
        println!("\n--- digest from field_skeleton.json ---\n{digest_before}");
        println!("--- digest from the atlas atoms ---\n{digest_after}");
    }
    if report.written == 0 && report.already_present == 0 {
        eprintln!("error: the skeleton projected no atoms — see the counts above");
        return 1;
    }
    0
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn print_usage() {
    eprintln!(
        "Usage: svrn enrich field-atoms <corpus> [--into <corpus>] [--show-digest]\n\n\
         Project the corpus's EXISTING field_skeleton.json into `Question` and\n\
         `Position` atoms in an atlas, so the ambient \"Field guide\" digest is\n\
         rendered from the atlas rather than from a parallel JSON file.\n\
         No extraction, no inference, no re-embed. Idempotent: an atom the\n\
         atlas already holds is skipped.\n\n\
         --into <corpus>   write the atoms into ANOTHER corpus's atlas — how a\n\
                           fixture copy is built without touching a control.\n\
         --show-digest     print both digests, v1 file and atlas atoms, in full.\n\n\
         Writes NO ANN seed row: the walk's seed space is unchanged, so a\n\
         retrieval lane run before and after this moves only for other reasons."
    );
}
