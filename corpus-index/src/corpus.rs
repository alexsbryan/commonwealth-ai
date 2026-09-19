// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Corpus`] — one installed corpus, named and located. corpus-engine's
//! published noun for "which corpus, and where does it live".
//!
//! # The leak this closes, measured
//!
//! `nc-boundary.py` says 337 TYPES cross into corpus-engine. It cannot see the
//! dependency that is actually widest, because that one is carried by string
//! literals. Counted on this tree, 2026-08-20:
//!
//! | convention | corpus-engine/src | sovereign | commonwealth |
//! |---|---|---|---|
//! | `.join("_corpus_meta.json")` | 62 | 63 | 22 |
//! | `format!("{id}-partition-{node}")` | 6 | 16 | 13 |
//!
//! **One filename. A hundred and fifty-six deciders. No constant anywhere.**
//! Every one of those sites has memorised corpus-engine's directory layout, so
//! renaming that file — or introducing a second index generation, or moving the
//! meta inside the Lance dataset — is a hundred-and-fifty-six-site change
//! across three workspaces. That is what "extending one domain requires reading
//! the others" looks like in the concrete, and no type-counting instrument
//! reports it (ARCH §10.6: one decider, one name).
//!
//! # Why this is not a facade over `CorpusEngine`
//!
//! A `Corpus` needs no engine. It is an id and a directory, and it answers the
//! questions those two determine — where the canonical directory is, where this
//! node's partition is, where the meta sits, is it installed. `CorpusEngine`
//! keeps the operations that genuinely need the engine's configuration (embed
//! functions, the recipe registry, the index cache) and its two path methods
//! now DELEGATE here rather than carrying a second copy of the join.
//!
//! So this does not wrap the god object; it takes a responsibility off it and
//! puts that responsibility somewhere a consumer can hold without holding a
//! 102-method engine.
//!
//! # Identity is `kernel_types::CorpusId`
//!
//! Not a `String`. `CorpusId` is non-empty by construction, which is a real
//! invariant: an empty corpus id currently reads as "all corpora" at some call
//! sites and "no corpus" at others. Taking the kernel's type here is also the
//! first load-bearing use of `kernel-types` from a product domain — the kernel
//! crate has existed since rung 1 with nothing in it that anyone spoke.

use std::path::{Path, PathBuf};

use kernel_types::CorpusId;

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::types::IndexInfo;

/// The per-corpus metadata sidecar, written at the root of every index
/// directory. **The** spelling of this filename — see the module docs for what
/// it cost to not have this constant.
pub const CORPUS_META_FILENAME: &str = "_corpus_meta.json";

/// Infix in a partition directory name: `<id>-partition-<node>`.
const PARTITION_INFIX: &str = "-partition-";

/// One installed corpus: which corpus, and which index root it lives under.
///
/// Cheap to build and to clone — two owned values, no handles, no IO. Opening
/// the index is an explicit [`Corpus::open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corpus {
    id: CorpusId,
    index_dir: PathBuf,
}

impl Corpus {
    /// Name a corpus under an index root.
    pub fn at(index_dir: impl Into<PathBuf>, id: CorpusId) -> Self {
        Self {
            id,
            index_dir: index_dir.into(),
        }
    }

    /// Name a corpus from a bare string id. `None` on an empty or
    /// whitespace-only id — **refused, not normalised to some default corpus**
    /// (ARCH §18.3: absence is reported, never defaulted). Callers that hold a
    /// `&str` from a CLI flag or a wire field want this one.
    pub fn named(index_dir: impl Into<PathBuf>, id: &str) -> Option<Self> {
        CorpusId::new(id).map(|id| Self::at(index_dir, id))
    }

    pub fn id(&self) -> &CorpusId {
        &self.id
    }

    /// The index root every corpus on this node sits under.
    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// The canonical (post-merge, fully committed) directory:
    /// `<index_dir>/<id>`. Reads — search, status, info — land here.
    pub fn root(&self) -> PathBuf {
        self.index_dir.join(self.id.as_str())
    }

    /// This corpus's partition directory for `node`:
    /// `<index_dir>/<id>-partition-<node>`.
    ///
    /// Every in-progress ingest writes to a partition; the canonical directory
    /// is materialised only by the finalise/merge step, never by a direct
    /// ingest write.
    pub fn partition(&self, node: &str) -> PathBuf {
        self.index_dir
            .join(format!("{}{PARTITION_INFIX}{node}", self.id))
    }

    /// The filename prefix every partition of this corpus shares — for callers
    /// enumerating or sweeping `<id>-partition-*`.
    pub fn partition_prefix(&self) -> String {
        format!("{}{PARTITION_INFIX}", self.id)
    }

    /// Metadata sidecar inside the canonical directory.
    pub fn meta_path(&self) -> PathBuf {
        Self::meta_in(self.root())
    }

    /// Metadata sidecar inside an arbitrary index directory — a partition, a
    /// shard, an unpacked snapshot. The ONE join for this filename; a caller
    /// holding a directory rather than a `Corpus` reaches this instead of
    /// retyping the literal.
    pub fn meta_in(dir: impl AsRef<Path>) -> PathBuf {
        dir.as_ref().join(CORPUS_META_FILENAME)
    }

    /// True when the canonical directory carries a metadata sidecar.
    ///
    /// Deliberately not `root().exists()`: an ingest that died after
    /// `create_dir_all` leaves a directory that is not a corpus, and every
    /// caller in the tree that tested directory existence was testing the
    /// wrong thing (`sharding.rs:88` says so in its own comment).
    pub fn is_installed(&self) -> bool {
        self.meta_path().is_file()
    }

    /// Open the canonical index for reading.
    pub async fn open(&self) -> Result<CorpusIndex> {
        CorpusIndex::open(&self.root()).await
    }

    /// Open and describe in one step.
    pub async fn info(&self) -> Result<IndexInfo> {
        self.open().await?.info().await
    }
}

impl std::fmt::Display for Corpus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Corpus {
        Corpus::named("/idx", "wikipedia").expect("non-empty id")
    }

    #[test]
    fn an_empty_id_is_refused_not_defaulted() {
        assert!(Corpus::named("/idx", "").is_none());
        assert!(Corpus::named("/idx", "   ").is_none());
        assert!(Corpus::named("/idx", "wikipedia").is_some());
    }

    #[test]
    fn the_layout_is_pinned_here_and_nowhere_else() {
        let c = corpus();
        assert_eq!(c.root(), PathBuf::from("/idx/wikipedia"));
        assert_eq!(
            c.partition("node-abc"),
            PathBuf::from("/idx/wikipedia-partition-node-abc")
        );
        assert_eq!(c.partition_prefix(), "wikipedia-partition-");
        assert_eq!(
            c.meta_path(),
            PathBuf::from("/idx/wikipedia/_corpus_meta.json")
        );
        assert_eq!(
            Corpus::meta_in("/anywhere"),
            PathBuf::from("/anywhere/_corpus_meta.json")
        );
    }

    /// A partition path must be reachable from the prefix, so a sweep over
    /// `<id>-partition-*` and a lookup of one node's partition cannot drift
    /// apart — they used to be two independent `format!`s.
    #[test]
    fn the_prefix_and_the_partition_path_agree() {
        let c = corpus();
        let p = c.partition("node-abc");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&c.partition_prefix()), "{name}");
    }

    #[test]
    fn installed_means_the_meta_is_there_not_that_the_dir_is() {
        let dir = tempfile::tempdir().unwrap();
        let c = Corpus::named(dir.path(), "wikipedia").unwrap();
        assert!(!c.is_installed(), "nothing on disk yet");
        std::fs::create_dir_all(c.root()).unwrap();
        assert!(
            !c.is_installed(),
            "a bare directory is a dead ingest, not a corpus"
        );
        std::fs::write(c.meta_path(), "{}").unwrap();
        assert!(c.is_installed());
    }
}
