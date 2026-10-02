// SPDX-License-Identifier: AGPL-3.0-or-later
//! Checked-in atlas fixtures for the svrn crates' tests (phase-b-48).
//!
//! svrn's `AtlasContextManager` opens atlas stores through this leaf's
//! openers, not through `AtlasPort`, and only ingest writes stores. So the
//! stores its tests open are checked in here, beside the reader of their
//! format, and have one writer: corpus-engine's `atlas_store_fixtures` test
//! regenerates them (`#[ignore]`d) and asserts that a freshly written store
//! and the checked-in one read the same. A store-format change turns that
//! test red.
//!
//! [`copy_store_fixture`] is the only path to the stores: a test copies one
//! into its own directory and never opens the checked-in bytes in place.

use std::io;
use std::path::{Path, PathBuf};

/// An empty atom store: `atoms.json` (no atoms) + `atoms.lance` + `edges.csr`.
pub const ATOM_STORE: &str = "atomish";

/// A wiki-class store: two articles, Alpha linking to Beta, written as
/// `articles.lance` + `edges.lance` under the corpus id `wikish`.
pub const WIKI_STORE: &str = "wikish";

/// The atom id of the wiki store's Alpha article
/// (`wiki_atom_id("Alpha", "wikish")`, asserted by the engine-side test).
pub const WIKI_ALPHA_ATOM_ID: &str = "entity-53ac3c012920dadb";

/// One typed-extension response envelope with one collection of each kind,
/// the canned model output both sides of the typed-extension tests use.
pub const ARGUMENTATIVE_ENVELOPE: &str = include_str!("../testdata/argumentative_envelope.json");

/// Where the checked-in store `name` lives. Derived from this leaf's own
/// manifest dir; the regenerating test writes here and nothing else does.
pub fn store_fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("stores")
        .join(name)
}

/// Copy the checked-in store `name` into `corpus_dir` (it gains the
/// fixture's `atlas/` tree). `Err` names the fixture when it is absent.
pub fn copy_store_fixture(name: &str, corpus_dir: &Path) -> io::Result<()> {
    let src = store_fixture_dir(name);
    if !src.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("atlas store fixture `{name}` not at {}", src.display()),
        ));
    }
    copy_tree(&src, corpus_dir)
}

fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}
