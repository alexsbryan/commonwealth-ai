// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Op`] and [`Oplog`] — one append-only journal, three tenants.
//!
//! # Why this module exists
//!
//! Measured 2026-08-20: corpus-engine held **three** append-only-JSONL logs
//! with the same envelope, the same file IO and the same reader, written three
//! times in three sibling directories. The code said so itself —
//! `enrichment/governance.rs` carried the section header
//! `// ── Persistence (mirrors reconciliation/oplog.rs conventions) ─`, and
//! `meta_atlas/bridge/edges.rs` opened with "The oplog mirrors the *discipline*
//! of `crate::enrichment::reconciliation::oplog`". Mirroring a discipline by
//! retyping it is how a discipline drifts.
//!
//! | was | entry | store | had id? | had actor? | had version gate? |
//! |---|---|---|---|---|---|
//! | governance | `GovernanceOp` | `GovernanceOplog` | yes | yes | yes |
//! | reconciliation | `OplogEntry` | `OplogWriter` + `OplogReader` | **no** | **no** | **no** |
//! | meta-atlas bridge | `BridgeOp` | `BridgeOplog` | **no** | **no** | **no** |
//!
//! The asymmetry was maturity, not concept: governance is the complete
//! version and the two younger logs are the same envelope with the identity
//! and attribution half never built. That absence was not cosmetic —
//! `reconciliation::OpKind::Split` documented itself as reversible by "walking
//! backwards finding the matching `Merge`", which is unimplementable without
//! an id to match on. Governance already had the answer
//! (`GovernanceOpKind::Revert { targets: Vec<OpId> }`).
//!
//! Seven types collapse to two here, and the two younger logs inherit the id,
//! the attribution and the version gate they were each missing.
//!
//! # What an `Op` is, and what it deliberately is not
//!
//! The envelope carries **provenance only**: what this act is identified as,
//! when it happened, who did it, and which line format it was written in. The
//! act itself — and its rationale, its evidence, its subjects — belongs to the
//! tenant's `K`. That cut is why governance can keep `rationale` required on
//! `AcceptTension` and optional elsewhere while the other two tenants carry
//! one envelope-level rationale: the envelope does not have an opinion.
//!
//! `K` is `#[serde(flatten)]`ed, so a line reads as one flat object and the
//! two tenants whose lines already looked like that keep their wire form:
//!
//! ```json
//! {"id":"gov-2f1c…","v":1,"ts_unix":1787,"actor":"human:alex","op":"supersede","new_rule":"…"}
//! ```
//!
//! # Identity is a content hash, and there is one derivation
//!
//! [`OpId`] is `<prefix>-<16 hex>` where the hex is the truncated BLAKE3 of
//! `<prefix>|<ts>|<actor>|<body-json>` — so the id a `Revert` targets is
//! reproducible from the log bytes alone, with no positional or external
//! counter to drift (ARCH §7.5: identity from essence, never a counter or an
//! address). Before this module that derivation existed once, privately, in
//! `governance.rs`; the other two logs had no ids at all. It is now the one
//! decider (ARCH §10.6) and every tenant reaches it.
//!
//! # Naming — why `Op` and not `Record`
//!
//! Work order `nc-5-corpus` proposed minting `Record` ("an immutable fact with
//! provenance, id = ContentHash") as the register noun for this shape. The
//! order's own not-worth-continuing-if clause fired against it: a census of the
//! 48 first-party `*Record` definitions splits 21 append-only facts against 21
//! store/wire row DTOs, and `id = ContentHash` is true of about three of the
//! forty-eight. Naming this `Record` would have annexed forty-five types that
//! do not share the property. `Op`/`Oplog` is the word the three call sites
//! already used, so this converges the tree's own vocabulary rather than
//! introducing a fourth (ARCH §19).

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// What a tenant must declare to get a journal: where its lines live, which
/// line format the current writer emits, and the prefix its ids wear.
///
/// A trait rather than three constructor arguments so the three facts cannot
/// be supplied inconsistently at two call sites of the same log (ARCH §10.6),
/// and so `Oplog::<K>::new(dir)` needs nothing but the directory.
pub trait Journaled: Serialize + DeserializeOwned {
    /// Basename of the JSONL file, joined onto the directory given to
    /// [`Oplog::new`].
    const FILE: &'static str;
    /// Short, stable id prefix — `"gov"`, `"recon"`, `"bridge"`. Part of the
    /// hashed input, so ids from two tenants can never collide even if the
    /// same body were written at the same second by the same actor.
    const ID_PREFIX: &'static str;
    /// Line format version this build writes. Bump only when a reader must
    /// opt in to new semantics; [`Oplog::read_all`] skips lines declaring a
    /// higher `v` rather than silently misreading them.
    const VERSION: u32 = 1;
    /// Short label for tracing and error text (`"governance_oplog"`).
    const LABEL: &'static str;
}

// ── Identity ─────────────────────────────────────────────────

/// Stable, content-addressed id for one op.
///
/// Opaque on purpose: the only way to mint one is [`Op::new`], which hashes
/// the act. `from_raw` exists for reading an id back off a log line or a CLI
/// argument, and is named so a reader sees no hashing happened here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpId(String);

impl OpId {
    pub fn from_raw(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OpId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── The envelope ─────────────────────────────────────────────

/// One line in an [`Oplog`] — an act (`kind`) plus its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "K: DeserializeOwned"))]
pub struct Op<K> {
    /// Content-addressed op id (see [`OpId`]). Stable across replays.
    pub id: OpId,
    /// Line format version. Always written; the read-side gate skips lines
    /// declaring a version this build does not understand.
    pub v: u32,
    /// When the act happened (Unix seconds).
    pub ts_unix: i64,
    /// Who performed it. `human:<name>` for an adjudication a person made,
    /// a machine label (`"ingest"`, `"reconcile:multi-origin"`) otherwise.
    /// Tenants that require human attribution enforce it themselves — see
    /// `governance::first_unattended_act`.
    pub actor: String,
    #[serde(flatten)]
    pub kind: K,
}

impl<K: Journaled> Op<K> {
    /// Build an op, deriving its content-addressed [`OpId`] from the
    /// (prefix, ts, actor, body) tuple.
    ///
    /// Two byte-identical acts at the same second by the same actor collide by
    /// design — callers append in real time, so this does not arise in
    /// practice (the same birthday-bound caveat the atom content-hash ids
    /// carry).
    pub fn new(kind: K, ts_unix: i64, actor: impl Into<String>) -> Self {
        let actor = actor.into();
        // serde_json writes fields in declaration order, so the body string —
        // and therefore the id — is deterministic across runs and builds.
        let body = serde_json::to_string(&kind).unwrap_or_default();
        let input = format!("{}|{ts_unix}|{actor}|{body}", K::ID_PREFIX);
        Self {
            id: OpId(format!(
                "{}-{}",
                K::ID_PREFIX,
                kernel_types::ContentHash::of_str(&input).short()
            )),
            v: K::VERSION,
            ts_unix,
            actor,
            kind,
        }
    }

    /// Build an op stamped with the current wall clock.
    pub fn now(kind: K, actor: impl Into<String>) -> Self {
        Self::new(kind, corpus_engine_yield::time::unix_now(), actor)
    }
}

// ── The journal ──────────────────────────────────────────────

/// Append-only JSONL journal at `<dir>/<K::FILE>`. One [`Op`] per line; the
/// file is the bytes-level record of every decision, and re-reading it is how
/// current state is derived rather than stored.
///
/// Reader and writer are one type. They were split for the reconciliation log
/// and joined for the other two, which bought nothing but a second name — an
/// `Oplog` holds a path and no handle, so it is as cheap to build as either
/// half was.
pub struct Oplog<K> {
    path: PathBuf,
    _kind: PhantomData<fn() -> K>,
}

impl<K: Journaled> Oplog<K> {
    /// Point at the log inside `dir`. Does not touch the filesystem — the
    /// directory is created lazily on first append, so a caller may build one
    /// for a corpus whose atlas dir does not exist yet.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join(K::FILE),
            _kind: PhantomData,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one op. Creates the parent directory on first write.
    pub fn append(&self, op: &Op<K>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let mut line = serde_json::to_string(op)
            .map_err(|e| Error::Extraction(format!("{}: serialise: {e}", K::LABEL)))?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(Error::Io)?;
        // ONE write for the whole logical line, so `O_APPEND`'s atomicity
        // covers it. Two calls (the body, then the newline) left a window in
        // which a second writer could interleave and split a line in half.
        f.write_all(line.as_bytes()).map_err(Error::Io)?;
        // Durability is the point of a journal. Without this a lid-slam or a
        // power cut loses the last ops that the caller was already told
        // succeeded — silently, which is the §18.3 shape.
        f.sync_data().map_err(Error::Io)?;
        tracing::debug!(
            log = K::LABEL,
            id = %op.id,
            actor = %op.actor,
            path = %self.path.display(),
            "oplog: append"
        );
        Ok(())
    }

    /// Append several ops in one open. Same semantics as calling
    /// [`Self::append`] per op; the file is opened once.
    pub fn append_all(&self, ops: &[Op<K>]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(Error::Io)?;
        let mut buf = String::new();
        for op in ops {
            let line = serde_json::to_string(op)
                .map_err(|e| Error::Extraction(format!("{}: serialise: {e}", K::LABEL)))?;
            buf.push_str(&line);
            buf.push('\n');
        }
        // Same single-write, then-fsync rule as `append`.
        f.write_all(buf.as_bytes()).map_err(Error::Io)?;
        f.sync_data().map_err(Error::Io)?;
        tracing::debug!(
            log = K::LABEL,
            ops = ops.len(),
            path = %self.path.display(),
            "oplog: append_all"
        );
        Ok(())
    }

    /// Every op in append order. A missing file is an empty log, not an error.
    ///
    /// Drops the skip list from [`Self::read_all_with_skips`]. That is the
    /// right call for a tenant deriving a *summary* it can restate later
    /// (governance, reconciliation, the meta-atlas bridge all re-run from the
    /// whole log). It is the WRONG call for a tenant whose answer is a number
    /// a person acts on — see the ring ledger, which reads the skips and
    /// reports them as gaps rather than confidently totalling a subset.
    pub fn read_all(&self) -> Result<Vec<Op<K>>> {
        self.read_all_with_skips().map(|(ops, _)| ops)
    }

    /// Every op in append order, **plus the lines this build could not use**.
    ///
    /// Two classes of line are skipped rather than defaulted (ARCH §18.3 —
    /// absence is reported, never defaulted): a line this build cannot parse,
    /// and a line declaring a `v` newer than `K::VERSION`. The second is the
    /// forward-compat gate: an older reader must not crash on a newer log, and
    /// must not silently misread it either.
    ///
    /// Both still warn. The warning was all a caller ever got, which means a
    /// caller could not *tell* — a log with a corrupt line and a log with none
    /// returned the identical value. Returning the skips is the difference
    /// between "here is the total" and "here is the total, and I could not
    /// read two lines."
    pub fn read_all_with_skips(&self) -> Result<(Vec<Op<K>>, Vec<SkippedLine>)> {
        if !self.path.exists() {
            return Ok((Vec::new(), Vec::new()));
        }
        let file = fs::File::open(&self.path).map_err(Error::Io)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        let mut skipped = Vec::new();
        for (lineno, line) in reader.lines().enumerate() {
            let line = line.map_err(Error::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            let line_no = lineno as u64 + 1;
            match serde_json::from_str::<Op<K>>(&line) {
                Ok(op) if op.v > K::VERSION => {
                    tracing::warn!(
                        log = K::LABEL,
                        path = %self.path.display(),
                        line = line_no,
                        v = op.v,
                        "oplog: skipping op from a newer format version"
                    );
                    skipped.push(SkippedLine::NewerVersion {
                        line: line_no,
                        v: op.v,
                    });
                }
                Ok(op) => out.push(op),
                Err(err) => {
                    tracing::warn!(
                        log = K::LABEL,
                        path = %self.path.display(),
                        line = line_no,
                        "oplog: malformed line skipped ({err})"
                    );
                    skipped.push(SkippedLine::Malformed {
                        line: line_no,
                        error: err.to_string(),
                    });
                }
            }
        }
        Ok((out, skipped))
    }
}

/// A line present in the journal that this build did not turn into an [`Op`].
///
/// Not an error — the read still succeeds, and every other line is returned.
/// It is the *reportable absence*: the caller now holds the evidence that its
/// answer is derived from a subset, and can decide whether that matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkippedLine {
    /// This build could not parse the line at all.
    Malformed { line: u64, error: String },
    /// The line declares a format version newer than `K::VERSION`, so reading
    /// it would be guessing at semantics this build does not have.
    NewerVersion { line: u64, v: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case")]
    enum Probe {
        Touch { what: String },
    }

    impl Journaled for Probe {
        const FILE: &'static str = "probe_oplog.jsonl";
        const ID_PREFIX: &'static str = "probe";
        const LABEL: &'static str = "probe_oplog";
    }

    fn touch(what: &str) -> Probe {
        Probe::Touch { what: what.into() }
    }

    #[test]
    fn id_is_derived_from_the_act_not_from_position() {
        let a = Op::new(touch("x"), 100, "human:alex");
        let b = Op::new(touch("x"), 100, "human:alex");
        let c = Op::new(touch("y"), 100, "human:alex");
        assert_eq!(a.id, b.id, "same act, same second, same actor => same id");
        assert_ne!(a.id, c.id, "a different act must not reuse the id");
        assert!(a.id.as_str().starts_with("probe-"));
    }

    #[test]
    fn the_prefix_is_hashed_so_two_tenants_cannot_collide() {
        // Same ts, same actor, same JSON body, different tenant prefix.
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(tag = "op", rename_all = "snake_case")]
        enum Other {
            Touch { what: String },
        }
        impl Journaled for Other {
            const FILE: &'static str = "other_oplog.jsonl";
            const ID_PREFIX: &'static str = "other";
            const LABEL: &'static str = "other_oplog";
        }
        let a = Op::new(touch("x"), 100, "ingest");
        let b = Op::new(
            Other::Touch {
                what: "x".to_string(),
            },
            100,
            "ingest",
        );
        assert_ne!(
            a.id.as_str()["probe-".len()..],
            b.id.as_str()["other-".len()..],
            "the hashed input must include the prefix, not only the body"
        );
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path().join("atlas"));
        let op = Op::new(touch("x"), 7, "human:alex");
        log.append(&op).unwrap();
        let back = log.read_all().unwrap();
        assert_eq!(back, vec![op]);
        assert!(log.path().ends_with("probe_oplog.jsonl"));
    }

    #[test]
    fn the_line_is_one_flat_object_with_the_kind_inlined() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path());
        log.append(&Op::new(touch("x"), 7, "ingest")).unwrap();
        let raw = fs::read_to_string(log.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        for key in ["id", "v", "ts_unix", "actor", "op", "what"] {
            assert!(v.get(key).is_some(), "line is missing {key}: {raw}");
        }
    }

    #[test]
    fn a_missing_log_reads_as_empty_not_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path().join("nope"));
        assert!(log.read_all().unwrap().is_empty());
    }

    /// The §18.3 property: a reader that skipped lines can SAY so. Before
    /// `read_all_with_skips` a corrupt log and a clean one were indistinguishable
    /// from the return value.
    #[test]
    fn the_skipped_lines_are_returned_not_only_logged() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path());
        let mut future = Op::new(touch("x"), 7, "ingest");
        future.v = Probe::VERSION + 1;
        log.append(&future).unwrap();
        log.append(&Op::new(touch("y"), 8, "ingest")).unwrap();
        fs::write(
            log.path(),
            format!(
                "{}\nnot json at all\n",
                fs::read_to_string(log.path()).unwrap().trim()
            ),
        )
        .unwrap();

        let (ops, skipped) = log.read_all_with_skips().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(skipped.len(), 2, "both classes of skip must be reported");
        assert!(matches!(
            skipped[0],
            SkippedLine::NewerVersion { line: 1, v } if v == Probe::VERSION + 1
        ));
        assert!(matches!(skipped[1], SkippedLine::Malformed { line: 3, .. }));
        // A clean log reports nothing, so a non-empty list is real signal.
        let clean: Oplog<Probe> = Oplog::new(dir.path().join("clean"));
        clean.append(&Op::new(touch("z"), 9, "ingest")).unwrap();
        assert!(clean.read_all_with_skips().unwrap().1.is_empty());
    }

    /// `O_APPEND` guarantees one `write(2)` lands whole. Two calls per logical
    /// line did not have that guarantee, so a concurrent writer could split a
    /// line across another's. Pinning the shape, not the race: every byte of a
    /// line — newline included — comes from one buffer.
    #[test]
    fn a_logical_line_including_its_newline_is_written_once() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path());
        log.append_all(&[
            Op::new(touch("a"), 1, "ingest"),
            Op::new(touch("b"), 2, "ingest"),
        ])
        .unwrap();
        let raw = fs::read_to_string(log.path()).unwrap();
        assert!(raw.ends_with('\n'), "every line is newline-terminated");
        assert_eq!(raw.lines().count(), 2);
        assert_eq!(log.read_all().unwrap().len(), 2);
    }

    #[test]
    fn a_line_from_a_newer_format_is_skipped_not_misread() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path());
        let mut future = Op::new(touch("x"), 7, "ingest");
        future.v = Probe::VERSION + 1;
        log.append(&future).unwrap();
        log.append(&Op::new(touch("y"), 8, "ingest")).unwrap();
        let back = log.read_all().unwrap();
        assert_eq!(back.len(), 1, "the future-version line must not be read");
        assert_eq!(back[0].ts_unix, 8);
    }

    #[test]
    fn a_malformed_line_is_skipped_and_the_rest_survive() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path());
        log.append(&Op::new(touch("x"), 7, "ingest")).unwrap();
        fs::write(
            log.path(),
            format!(
                "{}\nnot json at all\n",
                fs::read_to_string(log.path()).unwrap().trim()
            ),
        )
        .unwrap();
        assert_eq!(log.read_all().unwrap().len(), 1);
    }

    #[test]
    fn append_all_writes_every_op_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let log: Oplog<Probe> = Oplog::new(dir.path());
        let ops = vec![
            Op::new(touch("a"), 1, "ingest"),
            Op::new(touch("b"), 2, "ingest"),
        ];
        log.append_all(&ops).unwrap();
        assert_eq!(log.read_all().unwrap(), ops);
        // Empty batch must not create the file.
        let empty: Oplog<Probe> = Oplog::new(dir.path().join("empty"));
        empty.append_all(&[]).unwrap();
        assert!(!empty.path().exists());
    }
}
