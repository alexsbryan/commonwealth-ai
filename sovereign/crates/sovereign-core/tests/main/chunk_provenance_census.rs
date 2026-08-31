// SPDX-License-Identifier: AGPL-3.0-or-later
//! `adopted` for hazard 1 — how much pool content never passed an acquisition
//! door.
//!
//! `quality/TOPOLOGY.md` §10 phase 9 rung 9.1. §6 asks the register for an
//! `adopted` column whose value is "the count of remaining constructors other
//! than the canonical one", and states the reason plainly: `home = minted` is
//! true for `Evidence` and worth nothing, because nine other doors are open.
//! This file is that column for the retrieval pool, and it is the number the
//! rung drives to zero.
//!
//! # What is counted
//!
//! `corpus_engine::index::ChunkProvenance` has two arms. `Acquired` is
//! stamped by a door from the index's own facts and has **no public
//! constructor** — the compiler holds that half, so this census does not
//! re-check it. `Manufactured { producer }` is what a process writes when it
//! builds a chunk itself, and every one of those is a row here.
//!
//! A manufactured chunk is not a bug. An atlas atom, a conversation turn, a
//! RAPTOR rollup are all legitimately in the pool and legitimately not
//! citable. What was a bug is that they were **indistinguishable** from
//! content an index vouched for: provenance lived in
//! `metadata: HashMap<String, String>`, a missing key and a misspelled key
//! were the same value, and a manufactured chunk arrived with an empty bag.
//! That is hazard 1 — "a prompt fed by content that never passed `retrieve`" —
//! and it is why `CorpusIndex::retrieve` having zero production callers
//! mattered: the door existed and nothing walked through it.
//!
//! # Named failing input (ARCH §18.1)
//!
//! Add a `ScoredChunk { .. }` literal in production with a new producer name.
//! It fails here, and the author has to say what built it and why an
//! acquisition door cannot. Watched to fail before this file was kept.
//!
//! # This is a RATCHET
//!
//! The list may shrink freely — that is the rung landing. It fails on growth,
//! and on a producer disappearing without the baseline moving, because both
//! mean the census stopped describing the tree.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Production manufacturers, measured 2026-08-26 when the field was minted.
///
/// Each carries what it would take to become `Acquired`, because "on the list"
/// and "cannot come off the list" are different states and only one is work.
const MANUFACTURED: &[(&str, &str)] = &[
    // corpus-engine's own: an atlas entity description embedded as a virtual
    // chunk. Never was index content; the atlas is a derived surface.
    (
        "atlas_context_entity",
        "a derived atlas surface, not an indexed row",
    ),
    // A knowledge-atlas record composed from a corpus description plus
    // previews, inside the evidence loop.
    (
        "atlas_atom",
        "composed in-process from a corpus description",
    ),
    // A rendered turn from the current conversation, presented as a chunk so
    // the metalingual handler can cite 'this conversation'.
    ("conversation_turn", "conversation text, not corpus content"),
    // Atlas claim atoms injected as virtual chunks when a question is an
    // overview ask and the pool has no anchor.
    ("atom_enum_claim", "an atlas claim atom, not an indexed row"),
    // A conversation RAPTOR rollup: model-authored prose ABOUT conversation
    // text. The legacy bag said the same thing with metadata["source"]="raptor"
    // compared at three sites.
    (
        "raptor_summary",
        "model-authored summary; may orient, may not be quoted",
    ),
];

/// Acquisition doors for stores that are NOT a corpus index.
///
/// The other half of the ledger. `Acquisition::stamped` is `pub(crate)`, so
/// the compiler stops `sovereign` from inventing a custody — but a door added
/// inside corpus-engine hands one out, and a door that took a `custody`
/// argument would be that same public constructor wearing a door's name. Each
/// row is a door whose custody is fixed by WHICH DOOR IT IS, and adding one is
/// therefore a written decision rather than a new function.
const DOORS: &[(&str, &str)] = &[
    // The operator's own store: `Custody::Personal` source text, fixed. The
    // store carries no metadata bag, so custody is not read — it is a
    // property of the door, and misuse over-refuses (Personal is the most
    // restrictive released class) rather than leaking.
    (
        "acquired_from_estate",
        "the estate store, fixed at Personal",
    ),
    // The mesh reply path, opened 2026-08-26 by putting custody + grain on the
    // knowledge wire. Custody is JOINED with `Custody::Peer` rather than taken
    // from the peer, so a peer cannot talk its content down to a looser class;
    // grain travels, and absence reads as `Summary`. Both defaults preserve
    // exactly what a `Manufactured` mesh hit did before the door existed.
    (
        "acquired_from_peer",
        "the mesh wire, custody joined with Peer",
    ),
];

/// Producers that are not pool content and are deliberately not on the ratchet.
const NON_CONTENT: &[&str] = &[
    // Test fixtures. Nothing acquired them and nothing should.
    "test_fixture",
    // `ScoredChunk`'s `Deserialize` is vestigial (no production path
    // deserializes one). It still has to yield a value, and the honest one is
    // "this process did not acquire it".
    "deserialized",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root is four levels above sovereign-core")
        .to_path_buf()
}

/// Every `ChunkProvenance::manufactured("…")` / `Manufactured { producer: "…" }`
/// in first-party source, minus test modules.
fn producers(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for area in ["corpus-engine/src", "sovereign/crates", "studio/crates"] {
        walk(&root.join(area), &mut out);
    }
    out
}

fn walk(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target" || n == "tests") {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let Ok(src) = std::fs::read_to_string(&p) else {
                continue;
            };
            // Cut at the first `#[cfg(test)]` — fixtures below it are not
            // production content, and counting them would make the ratchet
            // fire on test hygiene rather than on the hazard.
            let prod = match src.find("#[cfg(test)]") {
                Some(i) => &src[..i],
                None => &src[..],
            };
            // Tolerant of the line break rustfmt inserts between the call
            // and its argument — a scan that only matched `manufactured("`
            // missed `manufactured(\n    "atom_enum_claim",` and reported a
            // producer as GONE when it was three lines below. That is the
            // instrument lying in the direction of good news (ARCH §18.4).
            // `manufactured_summary(` is NOT matched by `manufactured(` —
            // the char after `manufactured` is `_`, not `(` — so the scan
            // would have reported `raptor_summary` as GONE the moment it
            // gained a grain. Same instrument defect as the rustfmt line
            // break above, in the same direction: good news that is not true
            // (ARCH §18.4).
            for needle in ["manufactured(", "manufactured_summary(", "producer:"] {
                let mut at = 0usize;
                while let Some(rel) = prod[at..].find(needle) {
                    let start = at + rel + needle.len();
                    at = start;
                    let rest = prod[start..].trim_start();
                    let Some(lit) = rest.strip_prefix('"') else {
                        continue;
                    };
                    let name: String = lit.chars().take_while(|c| *c != '"').collect();
                    if !name.is_empty() && !NON_CONTENT.contains(&name.as_str()) {
                        out.insert(name);
                    }
                }
            }
        }
    }
}

#[test]
fn the_scan_finds_the_producers_it_is_meant_to_count() {
    // Instrument first (ARCH §18.4). A scan that finds nothing would report a
    // closed hazard, which is the failure mode this whole phase exists to
    // treat.
    let found = producers(&repo_root());
    assert!(
        found.len() >= 4,
        "found only {} manufactured producers — the scan is broken, not the tree. \
         A census that finds nothing reports an invariant it did not measure.\n{found:#?}",
        found.len()
    );
}

#[test]
fn no_new_pool_content_bypasses_an_acquisition_door() {
    let found = producers(&repo_root());
    let known: BTreeSet<String> = MANUFACTURED.iter().map(|(n, _)| n.to_string()).collect();

    let new: Vec<&String> = found.difference(&known).collect();
    assert!(
        new.is_empty(),
        "new manufactured chunk producer(s). Each one is content that will reach a prompt \
         without passing an acquisition door (TOPOLOGY hazard 1). Either acquire it through a \
         `corpus_engine::index::CorpusIndex` door, or add it to MANUFACTURED with what would \
         take it off the list.\n{new:#?}"
    );

    let gone: Vec<&String> = known.difference(&found).collect();
    assert!(
        gone.is_empty(),
        "these producers are on the list and no longer in the tree. That is PROGRESS — delete \
         them from MANUFACTURED in the same commit, so the count keeps describing the code \
         rather than describing what it used to be.\n{gone:#?}"
    );
}

#[test]
fn sovereign_cannot_stamp_an_acquisition() {
    // The compiler holds this — `Acquisition::stamped` is `pub(crate)` to
    // corpus-engine — so what this guards is the SPELLING of the invariant
    // surviving a refactor that makes it public "just for a test".
    let root = repo_root();
    let mut offenders = Vec::new();
    for area in ["sovereign/crates", "studio/crates", "commonwealth/crates"] {
        let mut hits = BTreeSet::new();
        collect_literal(&root.join(area), "Acquisition::stamped(", &mut hits);
        offenders.extend(hits);
    }
    assert!(
        offenders.is_empty(),
        "a crate outside corpus-engine stamped an acquisition. Only an index knows what it \
         acquired; a caller that can stamp one can also stamp a fabricated one (ARCH §7).\n\
         {offenders:#?}"
    );
}

/// Every `pub fn acquired_from_*` in corpus-engine — the doors that hand an
/// `Acquired` stamp to a caller who is not an index.
fn doors(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk_doors(&root.join("corpus-engine/src"), &mut out);
    out
}

fn walk_doors(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target" || n == "tests") {
                continue;
            }
            walk_doors(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let Ok(src) = std::fs::read_to_string(&p) else {
                continue;
            };
            let needle = "pub fn acquired_from";
            let mut at = 0usize;
            while let Some(rel) = src[at..].find(needle) {
                let start = at + rel + "pub fn ".len();
                at = start;
                let name: String = src[start..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    out.insert(name);
                }
            }
        }
    }
}

#[test]
fn every_acquisition_door_is_a_written_decision() {
    let found = doors(&repo_root());
    let known: BTreeSet<String> = DOORS.iter().map(|(n, _)| n.to_string()).collect();

    // Self-checking instrument: a scan that found nothing would pass the
    // `new` assertion and report a closed hazard, so the `gone` assertion
    // below is what catches a broken scan — it fires when a declared door
    // cannot be found.
    let new: Vec<&String> = found.difference(&known).collect();
    assert!(
        new.is_empty(),
        "a new acquisition door hands an `Acquired` stamp to a caller that is not an \
         index. Say which store it speaks for and what custody that store fixes, in \
         DOORS. A door that takes a `custody` argument is a public constructor for \
         `Acquired` wearing a door's name (ARCH §7).\n{new:#?}"
    );

    let gone: Vec<&String> = known.difference(&found).collect();
    assert!(
        gone.is_empty(),
        "a declared door is not in the tree — either it was deleted (drop the row in \
         the same commit) or this scan stopped finding doors, which would make the \
         check above vacuous.\n{gone:#?}"
    );
}

/// The peer door must never LOOSEN on a peer that says nothing.
///
/// Failing input (ARCH §18.1): default the missing grain to `Grain::Leaf`, or
/// take the peer's custody instead of joining it. Either makes an un-upgraded
/// peer's hit more citable than it was as `Manufactured`, which is the
/// direction that fabricates.
#[test]
fn the_peer_door_refuses_on_absence_and_joins_on_presence() {
    use corpus_engine::index::ChunkProvenance;
    use kernel_types::{Custody, Grain};

    // An un-upgraded peer: no custody, no grain. Exactly as unquotable and as
    // unstamped as the `Manufactured` mesh hit it replaces.
    let silent = ChunkProvenance::acquired_from_peer("wikipedia", None, None);
    assert_eq!(
        silent.stamped_custody(),
        None,
        "absence must not engage the gate's custody machinery"
    );
    assert_eq!(silent.grain(), Grain::Summary);
    assert!(!silent.may_be_quoted());

    // A peer that says `public-web` still lands at `Peer` — the join carries
    // THIS node's fact, so the peer cannot talk its content down.
    let claimed = ChunkProvenance::acquired_from_peer(
        "wikipedia",
        Some(Custody::PublicWeb),
        Some(Grain::Leaf),
    );
    assert_eq!(claimed.stamped_custody(), Some(Custody::Peer));
    assert!(claimed.may_be_quoted(), "a peer-vouched leaf may be quoted");

    // …and a peer's PERSONAL material stays personal: the join is
    // max-restrictiveness, not "whatever this node would prefer".
    let personal =
        ChunkProvenance::acquired_from_peer("notes", Some(Custody::Personal), Some(Grain::Leaf));
    assert_eq!(personal.stamped_custody(), Some(Custody::Personal));
}

/// The estate door fixes its class. Failing input: give it a `custody`
/// parameter, or point it at any class other than `Personal`.
#[test]
fn the_estate_door_fixes_its_custody() {
    use corpus_engine::index::ChunkProvenance;
    use kernel_types::Custody;

    let p = ChunkProvenance::acquired_from_estate("estate-notes");
    assert_eq!(p.stamped_custody(), Some(Custody::Personal));
    assert_eq!(p.corpus(), Some("estate-notes"));
    // Estate documents are the operator's own text, not prose about it.
    assert!(p.may_be_quoted());
    assert_eq!(p.producer(), None);
}

/// `stamped_custody` is `Option` and must stay `Option`. Failing input:
/// collapse it to `custody()`, and every pre-custody turn — every pool where
/// nothing carries a stamp — engages the gate's custody machinery and
/// refuses (custody.md §4, red R-3).
#[test]
fn an_unstamped_pool_leaves_the_custody_machinery_disengaged() {
    use corpus_engine::index::ChunkProvenance;
    use kernel_types::Custody;

    let manufactured = ChunkProvenance::manufactured("atlas_atom");
    assert_eq!(manufactured.stamped_custody(), None);
    // …while the question "what class is this content" still answers with the
    // refusing value, because those are different questions.
    assert_eq!(manufactured.custody(), Custody::Unknown);
}

fn collect_literal(dir: &Path, needle: &str, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // `tests` too: this census file names the literal it forbids, and
            // a check that fails on its own text is not a check (ARCH §18.1).
            if p.file_name().is_some_and(|n| n == "target" || n == "tests") {
                continue;
            }
            collect_literal(&p, needle, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && std::fs::read_to_string(&p).is_ok_and(|s| s.contains(needle))
        {
            out.insert(p.display().to_string());
        }
    }
}
