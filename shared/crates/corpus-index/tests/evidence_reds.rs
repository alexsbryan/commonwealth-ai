// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Evidence::acquired` is crate-private, proven where it is defined.
//!
//! The fixture compiles as an EXTERNAL crate against `corpus_index`. It lived
//! in corpus-engine's `tests/evidence_reds.rs` until pb-ingest, where its
//! recorded stderr named `$WORKSPACE/shared/crates/corpus-index/src/index/evidence.rs`, a
//! path that holds only in this monorepo's layout: in the ingest lift sandbox
//! the same note reads `$WORKSPACE/crates/corpus-index/...` and the suite went
//! red outside the monorepo. Here the definition is in the crate under test,
//! which trybuild renders relative to it (`src/index/evidence.rs`) under any
//! layout.
//!
//! Regenerate after an intentional change to the type's surface:
//!
//! ```text
//! TRYBUILD=overwrite cargo test -p corpus-index --test evidence_reds
//! ```

#[test]
fn evidence_acquired_is_crate_private() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/evidence_acquired_is_crate_private.rs");
}
