// SPDX-License-Identifier: AGPL-3.0-or-later
//! Zero-load GGUF metadata reader.
//!
//! Reads a single well-known metadata key — `general.architecture` — straight
//! from a GGUF file's header, **without** mmap-ing the weights or spinning up
//! llama.cpp. The point is to decide model/hardware compatibility *before*
//! loading: some architectures (Gated DeltaNet / Mamba / RWKV) SIGSEGV inside
//! ggml's recurrent `SET` op during CPU prefill, and a crash in C is
//! unrecoverable in-process — so we must know the architecture and refuse the
//! load pre-flight rather than discover it by crashing. See [`crate::cpu_compat`].
//!
//! ## Format
//!
//! GGUF v2/v3 header (all little-endian):
//! ```text
//!   u32 magic == "GGUF"          (0x46554747 LE)
//!   u32 version                  (2 or 3)
//!   u64 tensor_count
//!   u64 metadata_kv_count
//!   metadata_kv_count × {
//!     string key                 (u64 len + utf8 bytes)
//!     u32   value_type
//!     value                      (typed; see `skip_value`)
//!   }
//! ```
//! We stream through the KV section (seeking past values we don't want) and
//! stop at `general.architecture`. Malformed input yields an `Err`, never a
//! panic — robustness is the whole reason this path exists.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// "GGUF" as a little-endian u32.
const GGUF_MAGIC: u32 = 0x4655_4747;

// gguf_metadata_value_type discriminants (ggml.h).
const T_UINT8: u32 = 0;
const T_INT8: u32 = 1;
const T_UINT16: u32 = 2;
const T_INT16: u32 = 3;
const T_UINT32: u32 = 4;
const T_INT32: u32 = 5;
const T_FLOAT32: u32 = 6;
const T_BOOL: u32 = 7;
const T_STRING: u32 = 8;
const T_ARRAY: u32 = 9;
const T_UINT64: u32 = 10;
const T_INT64: u32 = 11;
const T_FLOAT64: u32 = 12;

/// The metadata key that carries the model architecture (e.g. `"qwen35"`,
/// `"qwen3"`, `"llama"`).
pub const ARCH_KEY: &str = "general.architecture";

/// Read [`ARCH_KEY`] from a GGUF file without loading it.
///
/// - `Ok(Some(arch))` — the architecture string.
/// - `Ok(None)` — a valid GGUF header with no (string-typed) architecture key.
/// - `Err(_)` — I/O error or a malformed/oversized header. Never panics.
pub fn read_architecture(path: &Path) -> io::Result<Option<String>> {
    let mut r = BufReader::new(File::open(path)?);

    let magic = read_u32(&mut r)?;
    if magic != GGUF_MAGIC {
        return Err(invalid("not a GGUF file (bad magic)"));
    }
    let version = read_u32(&mut r)?;
    if !(2..=3).contains(&version) {
        return Err(invalid(&format!("unsupported GGUF version {version}")));
    }
    let _tensor_count = read_u64(&mut r)?;
    let kv_count = read_u64(&mut r)?;
    // A sane cap: real models carry dozens–hundreds of KV entries, never
    // millions. Guards a corrupt count from spinning the loop forever.
    if kv_count > 1_000_000 {
        return Err(invalid("implausible GGUF metadata_kv_count"));
    }

    for _ in 0..kv_count {
        let key = read_gguf_string(&mut r)?;
        let vtype = read_u32(&mut r)?;
        if key == ARCH_KEY {
            if vtype != T_STRING {
                // Present but not a string — treat as "unknown" rather than error.
                return Ok(None);
            }
            return Ok(Some(read_gguf_string(&mut r)?));
        }
        skip_value(&mut r, vtype)?;
    }
    Ok(None)
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_gguf_string<R: Read>(r: &mut R) -> io::Result<String> {
    let len = read_u64(r)?;
    // A key or scalar-string value larger than 64 KiB is a corrupt read.
    if len > 64 * 1024 {
        return Err(invalid("gguf string length out of range"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|_| invalid("gguf string not valid utf-8"))
}

/// Byte size of a fixed-width scalar value type, or `None` for variable-width
/// (string / array) or unknown types.
fn scalar_size(vtype: u32) -> Option<u64> {
    match vtype {
        T_UINT8 | T_INT8 | T_BOOL => Some(1),
        T_UINT16 | T_INT16 => Some(2),
        T_UINT32 | T_INT32 | T_FLOAT32 => Some(4),
        T_UINT64 | T_INT64 | T_FLOAT64 => Some(8),
        _ => None,
    }
}

/// Advance past one metadata value of `vtype` without materializing it. Uses
/// `Seek` for large arrays (e.g. a tokenizer's 150k-string vocab) so we never
/// read gigabytes to reach a key that follows it.
fn skip_value<R: Read + Seek>(r: &mut R, vtype: u32) -> io::Result<()> {
    if let Some(sz) = scalar_size(vtype) {
        r.seek(SeekFrom::Current(sz as i64))?;
        return Ok(());
    }
    match vtype {
        T_STRING => {
            let len = read_u64(r)?;
            r.seek(SeekFrom::Current(len as i64))?;
        }
        T_ARRAY => {
            let elem_type = read_u32(r)?;
            let count = read_u64(r)?;
            if let Some(sz) = scalar_size(elem_type) {
                r.seek(SeekFrom::Current(sz.saturating_mul(count) as i64))?;
            } else if elem_type == T_STRING {
                // Variable-width elements — walk each length and seek past it.
                for _ in 0..count {
                    let len = read_u64(r)?;
                    r.seek(SeekFrom::Current(len as i64))?;
                }
            } else {
                return Err(invalid("nested or unknown GGUF array element type"));
            }
        }
        _ => return Err(invalid(&format!("unknown GGUF value type {vtype}"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── tiny GGUF writer, just enough to exercise the reader ──────────────
    struct GgufBuilder {
        buf: Vec<u8>,
        kv_count: u64,
    }
    impl GgufBuilder {
        fn new() -> Self {
            let mut buf = Vec::new();
            buf.extend_from_slice(&GGUF_MAGIC.to_le_bytes());
            buf.extend_from_slice(&3u32.to_le_bytes()); // version
            buf.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
            buf.extend_from_slice(&0u64.to_le_bytes()); // kv_count placeholder
            Self { buf, kv_count: 0 }
        }
        fn key(&mut self, k: &str) {
            self.buf.extend_from_slice(&(k.len() as u64).to_le_bytes());
            self.buf.extend_from_slice(k.as_bytes());
        }
        fn str_kv(mut self, k: &str, v: &str) -> Self {
            self.key(k);
            self.buf.extend_from_slice(&T_STRING.to_le_bytes());
            self.buf.extend_from_slice(&(v.len() as u64).to_le_bytes());
            self.buf.extend_from_slice(v.as_bytes());
            self.kv_count += 1;
            self
        }
        fn u32_kv(mut self, k: &str, v: u32) -> Self {
            self.key(k);
            self.buf.extend_from_slice(&T_UINT32.to_le_bytes());
            self.buf.extend_from_slice(&v.to_le_bytes());
            self.kv_count += 1;
            self
        }
        fn str_array_kv(mut self, k: &str, items: &[&str]) -> Self {
            self.key(k);
            self.buf.extend_from_slice(&T_ARRAY.to_le_bytes());
            self.buf.extend_from_slice(&T_STRING.to_le_bytes());
            self.buf
                .extend_from_slice(&(items.len() as u64).to_le_bytes());
            for it in items {
                self.buf.extend_from_slice(&(it.len() as u64).to_le_bytes());
                self.buf.extend_from_slice(it.as_bytes());
            }
            self.kv_count += 1;
            self
        }
        fn finish(mut self) -> Vec<u8> {
            self.buf[16..24].copy_from_slice(&self.kv_count.to_le_bytes());
            self.buf
        }
    }

    fn write_tmp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.gguf");
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        (dir, p)
    }

    #[test]
    fn reads_architecture_when_first_key() {
        let bytes = GgufBuilder::new()
            .str_kv("general.architecture", "qwen35")
            .finish();
        let (_d, p) = write_tmp(&bytes);
        assert_eq!(read_architecture(&p).unwrap().as_deref(), Some("qwen35"));
    }

    #[test]
    fn reads_architecture_after_skipping_scalar_string_and_array_kvs() {
        // Force the reader to skip every non-string and variable-width case
        // before it reaches the arch key.
        let bytes = GgufBuilder::new()
            .u32_kv("general.file_type", 15)
            .str_kv("general.name", "Some-Model-4B")
            .str_array_kv("tokenizer.ggml.tokens", &["<s>", "hello", "world"])
            .str_kv("general.architecture", "llama")
            .finish();
        let (_d, p) = write_tmp(&bytes);
        assert_eq!(read_architecture(&p).unwrap().as_deref(), Some("llama"));
    }

    #[test]
    fn none_when_arch_absent() {
        let bytes = GgufBuilder::new().str_kv("general.name", "x").finish();
        let (_d, p) = write_tmp(&bytes);
        assert_eq!(read_architecture(&p).unwrap(), None);
    }

    #[test]
    fn err_on_bad_magic() {
        let (_d, p) = write_tmp(b"NOTGGUFatall............");
        assert!(read_architecture(&p).is_err());
    }

    #[test]
    fn err_on_truncated_header() {
        let (_d, p) = write_tmp(&GGUF_MAGIC.to_le_bytes());
        assert!(read_architecture(&p).is_err());
    }
}
