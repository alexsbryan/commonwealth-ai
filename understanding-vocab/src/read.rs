// SPDX-License-Identifier: AGPL-3.0-or-later
//! The read door — the constructors for the atlas products a consumer
//! outside `corpus-engine` reads.
//!
//! `atlas/atoms.json` and `atlas/edges.json` are the enrichment's published
//! artefacts. Until domains `dm-vocab-door-move` their readers lived in
//! `corpus-engine`'s `enrichment/atlas/writer.rs`, so every reader of a
//! product depended on the 196k-line engine that writes it, and nine callers
//! outside the engine parsed the files themselves instead (DE "The door").
//! The door is a product of the vocabulary, so it lives here beside the types
//! it returns; `corpus-engine` re-exports both functions at the historical
//! path, so nothing inside the monorepo had to change an import.
//!
//! The layout constant [`ATLAS_DIRNAME`] lives here too — DE "The read-port
//! leaf, measured again": "`ATLAS_DIRNAME` goes to the language, beside the
//! door." It names the directory the readers open, so a consumer that can name
//! the reader can name the directory without linking the engine.

use std::fs;
use std::io;
use std::path::Path;

use crate::atoms::{AtomsFile, AtomsFileWire};
use crate::edges::EdgesFile;

/// Directory name for atlas output under a corpus's index root.
/// Full path is `~/.svrnmesh/indexes/<corpus>/atlas/`.
pub const ATLAS_DIRNAME: &str = "atlas";

/// Read the atoms file back from disk. Used by Phase 6 / Phase 7
/// subcommands that run standalone after Phase 3b already wrote
/// the atlas directory.
///
/// The parse goes through [`AtomsFileWire`] — the crate-private twin that is
/// the only `Deserialize` for this file — so this function is the only
/// constructor that parses `atoms.json` (DM §10.5 "The correction").
pub fn read_atlas_atoms(atlas_dir: &Path) -> io::Result<AtomsFile> {
    let path = atlas_dir.join("atoms.json");
    let data = fs::read(&path)?;
    let wire: AtomsFileWire = serde_json::from_slice(&data).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("parse atoms.json: {e}"))
    })?;
    Ok(wire.into())
}

/// Read the edges file back from disk. Companion to
/// [`read_atlas_atoms`].
pub fn read_atlas_edges(atlas_dir: &Path) -> io::Result<EdgesFile> {
    // NOTE: callers on the hot atom-detail path must go through
    // `atlas_view::atom_detail::cached_edges`, not this directly — the
    // Wikipedia atlas ships a 1.3 GB edges.json and this does a full
    // fs::read + serde parse every call. See the edges cache there.
    let path = atlas_dir.join("edges.json");
    let data = fs::read(&path)?;
    serde_json::from_slice(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("parse edges.json: {e}")))
}
