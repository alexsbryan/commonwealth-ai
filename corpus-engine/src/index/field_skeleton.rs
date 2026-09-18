// SPDX-License-Identifier: AGPL-3.0-or-later
//! Field-model JSON persistence — host functions over an index directory.
//!
//! `FieldSkeleton` and its closure live in `understanding-vocab` (the
//! language). The IO that reads and writes them to an index directory is the
//! host's, and it is a free function rather than a `CorpusIndex` method since
//! domains `REVIEW-build-field-skeleton-vocab` took it off that type (DE "The
//! read-port leaf, measured again": "its four JSON methods leave
//! `CorpusIndex` as host functions over the index directory").

use std::path::Path;

use understanding_vocab::skeleton::FieldSkeleton;

use crate::error::{Error, Result};

/// The field-model pipeline's resume checkpoint. Named `_`-prefixed like every
/// other working file in an index directory (`_enrichment_state.json`,
/// `_enrichment_checkpoint.json`, `_raptor_checkpoint/`).
///
/// It is separate from [`FIELD_SKELETON_FILENAME`] because until ei-7b the
/// working state and the published artifact were the SAME file, which is how
/// one pipeline's checkpoint ended up being read at retrieval time by
/// `turn_prepass::splice_ambient_field_digests`.
pub const FIELD_CHECKPOINT_FILENAME: &str = "_field_skeleton_checkpoint.json";

/// The field-model JSON artifact. Written only by `JsonAndLance` domains.
pub const FIELD_SKELETON_FILENAME: &str = "field_skeleton.json";

/// Write the field-model pipeline's own resume checkpoint.
///
/// This is NOT a corpus artifact and nothing outside `field_engine.rs`
/// reads it. It exists because `FieldModelEngine`'s phase-1 resume needs
/// fields the atom vocabulary has no home for (position proponents, cluster
/// ids, centroid chunk ids, discovery confidence — see
/// `enrichment::field_atoms`), so the pipeline keeps its working state in
/// its own file and publishes atoms.
pub fn write_field_checkpoint(dir: &Path, skeleton: &FieldSkeleton) -> Result<()> {
    let path = dir.join(FIELD_CHECKPOINT_FILENAME);
    let json =
        serde_json::to_string_pretty(skeleton).map_err(|e| Error::Serialization(e.to_string()))?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Read the field-model pipeline's resume checkpoint, falling back to a
/// `field_skeleton.json` when no checkpoint exists.
///
/// The fallback is what lets an interrupted pre-ei-7b run resume after the
/// upgrade instead of restarting phase 1 from nothing, and it is also the
/// right read for a `JsonAndLance` domain, whose artifact IS that file.
pub fn load_field_checkpoint(dir: &Path) -> Result<Option<FieldSkeleton>> {
    match read_skeleton_json(&dir.join(FIELD_CHECKPOINT_FILENAME))? {
        Some(s) => Ok(Some(s)),
        None => load_field_skeleton(dir),
    }
}

/// Write the field skeleton JSON artifact.
///
/// The terminal artifact of a `SkeletonStorage::JsonAndLance` domain only
/// — the three KnowledgeView domains, whose reader
/// (`sovereign-tools::knowledge_view::manager`) has not been ported. A
/// `SkeletonStorage::AtlasAtoms` domain publishes atoms instead and never
/// reaches here (ei-7b).
pub fn write_field_skeleton(dir: &Path, skeleton: &FieldSkeleton) -> Result<()> {
    let path = dir.join(FIELD_SKELETON_FILENAME);
    let json =
        serde_json::to_string_pretty(skeleton).map_err(|e| Error::Serialization(e.to_string()))?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load the field skeleton JSON artifact if it exists.
///
/// Readers: the KnowledgeView manager and its cross-view digest, the
/// desktop budget probe, `sovereign-tools::epistemic`, the one-shot
/// `enrich field-atoms` migration, and [`load_field_checkpoint`]'s
/// fallback. For an `AtlasAtoms` domain this file is a pre-ei-7b leftover
/// and the live field model is in the atlas.
pub fn load_field_skeleton(dir: &Path) -> Result<Option<FieldSkeleton>> {
    read_skeleton_json(&dir.join(FIELD_SKELETON_FILENAME))
}

fn read_skeleton_json(path: &Path) -> Result<Option<FieldSkeleton>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    let skeleton = serde_json::from_str(&raw).map_err(|e| {
        Error::Serialization(format!("Bad field skeleton at {}: {e}", path.display()))
    })?;
    Ok(Some(skeleton))
}
