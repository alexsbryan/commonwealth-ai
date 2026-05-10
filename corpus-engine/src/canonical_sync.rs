//! Canonical-index sync: pack/unpack helpers for cross-peer transfer.
//!
//! Phase 6 of the resilience track. The mesh's `auto_recover` path
//! and the `sovereign corpus pull` CLI both need to ship a complete
//! canonical index — `<index_dir>/<corpus_id>/` — over HTTP and
//! reconstruct it on the receiving node. This module is the
//! transport-agnostic packing primitive: it produces a tar stream
//! compressed with zstd, and the inverse extractor.
//!
//! ## Why tar+zstd
//!
//! - **tar**: handles the directory walk, preserves file modes /
//!   timestamps, and tolerates LanceDB's mix of small JSON files
//!   and large fragment shards in one archive. The pure-Rust `tar`
//!   crate's streaming writer doesn't need the size up front, which
//!   matters when the canonical contains 100s of fragment files
//!   none of which we want to stat-and-buffer twice.
//! - **zstd**: 3–5× compression on LanceDB binary data in practice,
//!   ~600 MB/s compression speed at the default level on a modern
//!   CPU. The `zstd` crate exposes a `Encoder<W>` that fits the
//!   `Write` trait, so we can chain `tar::Builder<Encoder<W>>` and
//!   pipe straight through.
//!
//! ## Streaming model
//!
//! Both pack and unpack are synchronous functions that take a
//! `Write` / `Read` respectively. Callers that want async streaming
//! (the HTTP server, the HTTP client) wrap them with the standard
//! `tokio_util::io::SyncIoBridge` inside a `spawn_blocking` task.
//! Keeping this module sync-only avoids pulling tokio into a
//! pure-data crate.

use std::io::{Read, Write};
use std::path::Path;

use crate::error::{Error, Result};

/// Pack the canonical index directory at `canonical_path` into a
/// tar+zstd stream written to `writer`. Returns the number of
/// uncompressed bytes streamed (sum of file sizes plus tar
/// overhead) so callers can log throughput.
///
/// `compression_level` is passed through to zstd; a sensible
/// default is `0` (zstd's default level — currently 3, balances
/// ratio vs CPU). For Wikipedia-scale canonicals on a fast
/// network, level 1 is ~2× faster with a ~10% size penalty.
///
/// **Filesystem inputs**: walks every regular file under
/// `canonical_path` and records relative paths. Symlinks, sockets,
/// and devices are skipped (LanceDB never produces them; refusing
/// is safer than blindly preserving). The function does NOT fsync
/// — callers that need durability after a checksum-validated
/// receive should fsync the destination directory themselves.
///
/// Aborts on the first I/O error rather than continuing with a
/// half-built archive.
pub fn pack_canonical<W: Write>(
    canonical_path: &Path,
    writer: W,
    compression_level: i32,
) -> Result<u64> {
    if !canonical_path.is_dir() {
        return Err(Error::IndexNotFound(format!(
            "pack_canonical: not a directory: {}",
            canonical_path.display()
        )));
    }

    let encoder = zstd::Encoder::new(writer, compression_level)
        .map_err(|e| Error::Database(format!("pack_canonical: zstd init: {e}")))?
        // `auto_finish` ensures the encoder's frame is finalized on
        // drop. Without it, a panic mid-walk produces a truncated
        // zstd stream the receiver rejects.
        .auto_finish();

    let mut builder = tar::Builder::new(encoder);
    builder.mode(tar::HeaderMode::Deterministic);

    // Walk relative to canonical_path so the archive contains
    // entries like `chunks.lance/_versions/...` rather than
    // absolute paths. The receiver unpacks into a fresh directory
    // under its own index_dir so this layout is what it needs.
    let mut bytes_in: u64 = 0;
    walk_and_append(&mut builder, canonical_path, canonical_path, &mut bytes_in)?;
    builder
        .finish()
        .map_err(|e| Error::Database(format!("pack_canonical: tar finish: {e}")))?;
    drop(builder); // Drops the auto_finish encoder, finalizing zstd.
    Ok(bytes_in)
}

fn walk_and_append<W: Write>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    dir: &Path,
    bytes_in: &mut u64,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        Error::Database(format!("pack_canonical: read_dir {}: {e}", dir.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            Error::Database(format!("pack_canonical: dir entry: {e}"))
        })?;
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(|e| {
            Error::Database(format!("pack_canonical: strip_prefix: {e}"))
        })?;

        let meta = entry.metadata().map_err(|e| {
            Error::Database(format!(
                "pack_canonical: metadata {}: {e}",
                path.display()
            ))
        })?;
        if meta.is_dir() {
            walk_and_append(builder, root, &path, bytes_in)?;
        } else if meta.is_file() {
            let mut header = tar::Header::new_gnu();
            header.set_size(meta.len());
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            let mut file = std::fs::File::open(&path).map_err(|e| {
                Error::Database(format!(
                    "pack_canonical: open {}: {e}",
                    path.display()
                ))
            })?;
            builder
                .append_data(&mut header, rel, &mut file)
                .map_err(|e| {
                    Error::Database(format!(
                        "pack_canonical: append {}: {e}",
                        rel.display()
                    ))
                })?;
            *bytes_in = bytes_in.saturating_add(meta.len());
        }
        // Symlinks and other special files: silently skipped.
        // LanceDB never produces them in a canonical index dir;
        // if a future format does, we can extend this branch.
    }
    Ok(())
}

/// Inverse of [`pack_canonical`]. Decompresses + extracts the
/// stream from `reader` into `dest_path` (which must NOT already
/// exist — the caller is responsible for atomic rename semantics
/// after a fingerprint-validated unpack).
///
/// Returns the number of uncompressed bytes written so the caller
/// can log throughput.
///
/// **Path safety**: the tar extractor refuses entries whose
/// normalized path escapes `dest_path` (e.g. `../../etc/passwd`).
/// `tar::Archive::set_overwrite(false)` is set so a duplicate
/// entry can't blast over an earlier one — defence-in-depth even
/// though our `pack_canonical` won't produce duplicates.
pub fn unpack_canonical<R: Read>(
    reader: R,
    dest_path: &Path,
) -> Result<u64> {
    if dest_path.exists() {
        return Err(Error::Database(format!(
            "unpack_canonical: refuses to overwrite existing {}",
            dest_path.display()
        )));
    }
    std::fs::create_dir_all(dest_path).map_err(|e| {
        Error::Database(format!(
            "unpack_canonical: mkdir {}: {e}",
            dest_path.display()
        ))
    })?;

    let decoder = zstd::Decoder::new(reader)
        .map_err(|e| Error::Database(format!("unpack_canonical: zstd init: {e}")))?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_overwrite(false);
    archive.set_preserve_permissions(false);

    let mut bytes_out: u64 = 0;
    let entries = archive
        .entries()
        .map_err(|e| Error::Database(format!("unpack_canonical: entries: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| {
            Error::Database(format!("unpack_canonical: entry: {e}"))
        })?;
        // Path safety check: tar::Entries::path returns the entry's
        // declared path; we additionally verify the canonicalised
        // unpack target falls under dest_path before unpacking.
        let entry_path = entry
            .path()
            .map_err(|e| Error::Database(format!("unpack_canonical: path: {e}")))?
            .into_owned();
        if entry_path.is_absolute() {
            return Err(Error::Database(format!(
                "unpack_canonical: refuses absolute path entry {}",
                entry_path.display()
            )));
        }
        if entry_path.components().any(|c| {
            matches!(c, std::path::Component::ParentDir)
        }) {
            return Err(Error::Database(format!(
                "unpack_canonical: refuses '..' in entry path {}",
                entry_path.display()
            )));
        }
        let target = dest_path.join(&entry_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Database(format!(
                    "unpack_canonical: mkdir {}: {e}",
                    parent.display()
                ))
            })?;
        }
        let written = entry.unpack(&target).map_err(|e| {
            Error::Database(format!(
                "unpack_canonical: unpack {}: {e}",
                target.display()
            ))
        })?;
        let _ = written; // Ok(()) variant
        bytes_out = bytes_out.saturating_add(entry.size());
    }
    Ok(bytes_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Round-trip: pack a tiny synthetic canonical, unpack into a
    /// fresh dir, byte-for-byte assert the file tree is identical.
    #[test]
    fn pack_unpack_round_trips_directory_tree() {
        let src = tempdir().unwrap();
        let canonical = src.path().join("test-corpus");
        fs::create_dir_all(canonical.join("chunks.lance/_versions")).unwrap();
        fs::write(canonical.join("_corpus_meta.json"), b"{\"corpus_id\":\"test\"}").unwrap();
        fs::write(canonical.join("chunks.lance/manifest.json"), b"manifest").unwrap();
        fs::write(canonical.join("chunks.lance/_versions/1.bin"), vec![0u8; 1024]).unwrap();
        fs::write(
            canonical.join("chunks.lance/_versions/2.bin"),
            vec![42u8; 2048],
        )
        .unwrap();

        let dst = tempdir().unwrap();
        let dst_canonical = dst.path().join("restored");

        // Pack into an in-memory buffer (small enough), then unpack.
        let mut buf: Vec<u8> = Vec::new();
        let bytes_in = pack_canonical(&canonical, &mut buf, 1).unwrap();
        assert!(bytes_in > 0);
        let bytes_out =
            unpack_canonical(buf.as_slice(), &dst_canonical).unwrap();
        assert!(bytes_out > 0);

        // File tree equality.
        let read =
            |p: &Path| std::fs::read(p).unwrap();
        assert_eq!(
            read(&canonical.join("_corpus_meta.json")),
            read(&dst_canonical.join("_corpus_meta.json"))
        );
        assert_eq!(
            read(&canonical.join("chunks.lance/manifest.json")),
            read(&dst_canonical.join("chunks.lance/manifest.json"))
        );
        assert_eq!(
            read(&canonical.join("chunks.lance/_versions/1.bin")),
            read(&dst_canonical.join("chunks.lance/_versions/1.bin"))
        );
        assert_eq!(
            read(&canonical.join("chunks.lance/_versions/2.bin")),
            read(&dst_canonical.join("chunks.lance/_versions/2.bin"))
        );
    }

    /// `unpack_canonical` refuses an existing destination — the
    /// caller's atomic-rename pattern (write to temp, validate
    /// fingerprint, rename) depends on this behaviour. Without it,
    /// a partial-overlap unpack could mix-and-match files.
    #[test]
    fn unpack_canonical_refuses_existing_dest() {
        let dir = tempdir().unwrap();
        let dst = dir.path().join("preexists");
        std::fs::create_dir_all(&dst).unwrap();
        let buf: Vec<u8> = vec![0; 128];
        let r = unpack_canonical(buf.as_slice(), &dst);
        assert!(r.is_err(), "expected refusal on existing dest");
    }

    /// `pack_canonical` errors when the source isn't a directory
    /// (e.g. caller passed a file by mistake or the canonical was
    /// removed mid-flight). We surface a clear IndexNotFound rather
    /// than blindly producing an empty archive.
    #[test]
    fn pack_canonical_errors_on_non_directory() {
        let dir = tempdir().unwrap();
        let bogus = dir.path().join("does-not-exist");
        let mut buf: Vec<u8> = Vec::new();
        let r = pack_canonical(&bogus, &mut buf, 0);
        assert!(r.is_err());
    }
}
