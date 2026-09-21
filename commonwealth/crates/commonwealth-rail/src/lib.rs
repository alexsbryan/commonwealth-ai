// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ring journal on disk — `<root>/rings/<namespace>/ring_oplog.jsonl`.
//!
//! [`commonwealth_rail_core`] is the FOLD: vocabulary, signing, admission,
//! the sync digest, and not one line of I/O. This crate is the half that
//! touches a filesystem — one writer per namespace, the door that signs, and
//! the peer-ingest path. Everything the core exports is re-exported here, so
//! an application names one crate and a peer that only needs the fold (canon)
//! names the other.
//!
//! # The door is strict about what the rail knows, and silent about the rest
//!
//! [`RingJournal::append`] refuses what the rail can judge: a payload with no
//! canonical form (see [`Payload`]), and an attempt to author under a key the
//! ring's own roster does not carry. It says nothing about whether an amount
//! is positive or a borrower exists, because it cannot — those are the app's,
//! and the app owns one validator that its own door and its own reducer both
//! call, exactly as this module used to.
//!
//! The second of those is [`RailError::NotInRoster`] rather than a sentence,
//! because the command that fixes it depends on whether this namespace's
//! roster is a file or is DERIVED ([`RingRail::roster_origin`]) — the door
//! does not know, and the renderer does.
//!
//! The journal is **truth**; the mesh store is only a transport buffer. In
//! production `MeshStore` is `in_memory()`, so anything treating it as
//! durable loses the log on restart.

pub use commonwealth_rail_core::*;

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

// The one type the core does NOT re-export: it is the writer, and nothing
// above this crate opens a journal directly.
use oplog::Oplog;

/// A namespace names a directory, so it may only be a plain name.
fn valid_namespace(ns: &str) -> bool {
    !ns.is_empty()
        && ns.len() <= 64
        && ns
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Every ring this node holds, as a directory. The ONE place the literal
/// `rings` is spelled — [`ring_dir`] and [`RingRail::namespaces`] are its two
/// callers and there is no third spelling in the workspace (ARCH §10.6).
fn rings_root(root: &Path) -> PathBuf {
    root.join("rings")
}

/// Where one ring namespace keeps its journal and its roster:
/// `<root>/rings/<namespace>/`.
///
/// A caller that holds a [`RingJournal`] reads [`RingJournal::dir`] or
/// [`RingJournal::roster_path`] instead — those are the same join and they
/// also carry the namespace check. This is for the caller that has a root and
/// a name and nothing else.
fn ring_dir(root: &Path, namespace: &str) -> PathBuf {
    rings_root(root).join(namespace)
}

// ── Where a roster comes from ────────────────────────────────

/// A roster that is computed rather than read from `roster.json`.
///
/// Most rings are written by hand from the CLI and their roster is a file. A
/// ring the *daemon* publishes to on its own has no hand to write one, and
/// its roster is derived from state the node already holds — the mesh's
/// membership, for `mesh-measurements`. That derivation lives in the
/// application, so it reaches the rail through this trait rather than the
/// rail knowing the application's nouns.
///
/// The read is a future because the state it derives from is behind an
/// async lock in the daemon; a blocking read there would be wrong on a
/// single-threaded runtime and only *usually* right elsewhere.
pub trait RosterSource: Send + Sync {
    fn roster(&self) -> Pin<Box<dyn Future<Output = Result<Roster, RailError>> + Send + '_>>;
}

/// Which reader answered [`RingRail::roster`], so the decision is visible at
/// `tracing=debug` and a caller that needs to know (the CLI refusing to write
/// a file nothing reads) can ask without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterOrigin {
    /// `<ring>/roster.json`, written by `svrn ring roster add`.
    File,
    /// A [`RosterSource`] installed for this namespace; the file is ignored.
    Derived,
}

// ── The rail's storage ───────────────────────────────────────

/// Every ring namespace this node holds, and the one key it signs with.
///
/// Installed once by the daemon. A namespace's journal is opened on first
/// touch and kept, so the write lock that serialises appends is per-namespace
/// and outlives a request — two ring apps do not contend, and one app's two
/// requests do.
pub struct RingRail {
    root: PathBuf,
    signer: Arc<dyn RingSigner>,
    open: Mutex<BTreeMap<String, Arc<RingJournal>>>,
    /// Namespaces whose roster is derived, and by what. Consulted at read
    /// time and never at open time, so installing a source after a journal
    /// was first touched still takes effect — boot order cannot leave a
    /// namespace reading the wrong roster.
    derived: Mutex<BTreeMap<String, Arc<dyn RosterSource>>>,
    /// Who is in a ring nobody narrowed: the answer for every namespace with
    /// neither a registered source nor a `roster.json`. Unset, such a ring
    /// reads its (absent, so empty) file, which is what it did before this
    /// existed.
    default: Mutex<Option<Arc<dyn RosterSource>>>,
}

/// Which of the three readers answers a namespace — ONE decider, read by both
/// [`RingRail::roster_origin`] and [`RingRail::roster`] so the origin a caller
/// is told and the roster it is admitted against cannot disagree.
enum Answerer {
    Registered(Arc<dyn RosterSource>),
    File,
    Default(Arc<dyn RosterSource>),
}

impl Answerer {
    fn origin(&self) -> RosterOrigin {
        match self {
            Answerer::File => RosterOrigin::File,
            Answerer::Registered(_) | Answerer::Default(_) => RosterOrigin::Derived,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Answerer::Registered(_) => "registered",
            Answerer::File => "file",
            Answerer::Default(_) => "default",
        }
    }
}

impl RingRail {
    pub fn new(root: impl Into<PathBuf>, signer: Arc<dyn RingSigner>) -> Self {
        Self {
            root: root.into(),
            signer,
            open: Mutex::new(BTreeMap::new()),
            derived: Mutex::new(BTreeMap::new()),
            default: Mutex::new(None),
        }
    }

    /// Answer every ring nobody narrowed with `source` — an app applies to
    /// everyone in the mesh until someone writes its `roster.json`.
    ///
    /// Precedence is registered ([`Self::derive_roster`]) > the file > this.
    /// The file outranks it because the hand roster IS the narrowing
    /// primitive; a registered source outranks the file because a namespace
    /// is registered precisely so that no file can narrow it. Installing a
    /// second default replaces the first.
    pub fn default_roster(&self, source: Arc<dyn RosterSource>) {
        *self.default.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
        tracing::debug!("ring rail: rings nobody narrowed derive their roster by default");
    }

    fn answerer(&self, namespace: &str) -> Answerer {
        if let Some(source) = self
            .derived
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(namespace)
            .cloned()
        {
            return Answerer::Registered(source);
        }
        // An unreadable path is NOT "no file": the file answers, and its
        // reader reports the error rather than the default admitting
        // everyone in its place.
        let file_present = RingJournal::open(&self.root, namespace)
            .map(|j| j.roster_path().try_exists().unwrap_or(true))
            .unwrap_or(false);
        let default = self
            .default
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match default {
            Some(source) if !file_present => Answerer::Default(source),
            _ => Answerer::File,
        }
    }

    pub fn signer(&self) -> &dyn RingSigner {
        self.signer.as_ref()
    }

    /// Declare that `namespace`'s roster is computed by `source`, not read
    /// from its `roster.json`.
    ///
    /// Installing a second source for the same namespace replaces the first
    /// rather than stacking: there is one answer to who is in a ring.
    pub fn derive_roster(
        &self,
        namespace: &str,
        source: Arc<dyn RosterSource>,
    ) -> Result<(), RailError> {
        if !valid_namespace(namespace) {
            return Err(RailError::BadNamespace(namespace.to_string()));
        }
        self.derived
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(namespace.to_string(), source);
        tracing::debug!(namespace, "ring rail: roster is derived for this namespace");
        Ok(())
    }

    /// Where `namespace`'s roster comes from, without reading it.
    pub fn roster_origin(&self, namespace: &str) -> RosterOrigin {
        self.answerer(namespace).origin()
    }

    /// The roster `journal` is admitted against — THE door, and the only
    /// reader a caller holding a journal should use (ARCH §10.6).
    ///
    /// Until this existed the append route, the log route and the sync-side
    /// prune each read the file directly, while the daemon's own namespace
    /// derived its roster somewhere else entirely. Two answers to who is in
    /// `mesh-measurements`: the file's (empty, so the door refused this
    /// node's own key and a peer's seal retired nothing) and the membership's
    /// (what every read actually rendered). One reader, and the file is the
    /// fallback rather than a competitor.
    pub async fn roster(&self, journal: &RingJournal) -> Result<Roster, RailError> {
        // The answerer holds clones, so no std lock is held across the await.
        let answerer = self.answerer(journal.namespace());
        let roster = match &answerer {
            Answerer::Registered(source) | Answerer::Default(source) => source.roster().await?,
            Answerer::File => journal.roster_file()?,
        };
        tracing::debug!(
            namespace = journal.namespace(),
            origin = ?answerer.origin(),
            answered = answerer.name(),
            people = roster.members.len(),
            "ring rail: roster read"
        );
        Ok(roster)
    }

    /// Every namespace this node holds a journal for, read from disk.
    ///
    /// From DISK and not from the open map, because on boot nothing has been
    /// touched yet — and boot is exactly when replication most needs the
    /// list. A node that came back from a week off has to offer its whole
    /// journal to peers before anyone asks it anything.
    ///
    /// A missing `rings/` directory is an empty list, not an error: a daemon
    /// that has never hosted a ring is a normal daemon.
    pub fn namespaces(&self) -> Result<Vec<String>, RailError> {
        namespaces_in(&self.root)
    }
}

/// Every ring namespace under `root`, without holding a [`RingRail`].
///
/// A reader that only wants to LIST rings has no business minting a signer,
/// and a second `root.join("rings")` somewhere else is how the literal gets a
/// second spelling (ARCH §10.6 — the module note above says this is the one
/// place it lives). `svrn mesh offers --why` is the caller that needed it:
/// resolving a remote seller's warrant means asking every ring this node
/// holds, and it holds no rail.
pub fn namespaces_in(root: &Path) -> Result<Vec<String>, RailError> {
    let dir = rings_root(root);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(RailError::Io(format!("{}: {e}", dir.display()))),
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        // A directory whose name this build would refuse to open is not a
        // namespace — skip it rather than surfacing a path we cannot use.
        .filter(|name| valid_namespace(name))
        .collect();
    out.sort();
    Ok(out)
}

impl RingRail {
    /// The journal for one namespace, opening it if this is the first touch.
    pub fn journal(&self, namespace: &str) -> Result<Arc<RingJournal>, RailError> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = open.get(namespace) {
            return Ok(existing.clone());
        }
        let journal = Arc::new(RingJournal::open(&self.root, namespace)?);
        open.insert(namespace.to_string(), journal.clone());
        Ok(journal)
    }
}

// ── What a compaction did ────────────────────────────────────

/// What one [`RingJournal::compact`] removed, and the floors it removed by.
///
/// A count and not a `()` because "the journal is now shorter" and "there was
/// nothing to shorten" are different facts, and a caller that cannot tell them
/// apart cannot report either honestly. `removed: 0` is a normal, successful
/// answer — it is what every ring that has never sealed gets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compaction {
    /// Lines deleted from the journal.
    pub removed: usize,
    /// Lines still on it afterwards.
    pub kept: usize,
    /// Gaps the journal had before and does not have now — refused lines that
    /// sat below a floor their claimed author authenticated. Reported because
    /// a gap vanishing is a change to what this node claims completeness over,
    /// and a destructive path may not make that change silently (ARCH §18.3).
    pub gaps_cleared: usize,
    /// The AUTHENTICATED floors this prune deleted below — [`admit`]'s own map,
    /// carried through rather than re-derived, so the number above and the
    /// reason for it cannot disagree.
    pub floors: Floors,
}

/// One [`RingJournal::seal`]: the seal act, and the prune it authorises.
///
/// **A seal and its prune are one act, and they are not one result.** The seal
/// is signed and on disk by the time the prune runs, so a refused prune is not
/// a failed seal — reporting it as one would tell a caller its seal did not
/// land when it did, and the retry would write a second one. So the pair is an
/// `Ok` carrying a `Result`: the caller has to look at `retired` to say what
/// happened, and cannot mistake "nothing was retired" for "the prune was
/// refused" (ARCH §18.3).
#[derive(Debug)]
pub struct Sealed {
    pub op: Op<SignedOp>,
    /// What [`RingJournal::compact`] did, or why it would not.
    pub retired: Result<Compaction, RailError>,
}

// ── The journal on disk ──────────────────────────────────────

// `RingJournal` lives in a sibling file: with `RingRail` it put this file into
// the 800-1200 approach band (ARCH §3.1). Re-exported, so the public path
// `commonwealth_rail::RingJournal` is unchanged.
mod journal;
pub use journal::RingJournal;

#[cfg(test)]
mod tests;
