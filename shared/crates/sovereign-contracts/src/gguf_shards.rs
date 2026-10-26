// SPDX-License-Identifier: AGPL-3.0-or-later
//! The file layout of a split GGUF: which files one model name denotes.

use std::path::Path;

/// Every sibling file name of a split GGUF (`<stem>-<idx>-of-<count>.gguf`),
/// including `file_name` itself, in shard order — `None` when the name is not
/// a split (or declares a single shard).
///
/// Pure name arithmetic, no I/O. This is the ONE definition of "what files does
/// this model name denote"; `shard_files`, `total_model_bytes`, and the mesh's
/// worker-side fetch all delegate here. It used to be copy-pasted in three
/// places, which is exactly the kind of drift that produced the 2026-07-19
/// empty-warm deadlock — the readers disagreeing about what "the whole model"
/// means is the bug class, so there is deliberately only one parser.
///
/// Detection is by naming convention, matching what `llama-gguf-split` emits.
/// We deliberately do NOT read `split.count` from the GGUF header: the callers
/// that need this (advertising files, summing bytes) must answer before opening
/// any file, and a header read would make the cheap path expensive.
pub fn split_shard_names(file_name: &str) -> Option<Vec<String>> {
    let of = file_name.rfind("-of-")?;
    let count: u32 = file_name
        .get(of + 4..)?
        .strip_suffix(".gguf")?
        .parse()
        .ok()?;
    let before = file_name.get(..of)?; // "<stem>-<idx>"
    let dash = before.rfind('-')?;
    let idx = before.get(dash + 1..)?;
    idx.parse::<u32>().ok()?; // validate numeric
    let width = idx.len();
    let stem = before.get(..dash)?;
    if count <= 1 {
        return None;
    }
    Some(
        (1..=count)
            .map(|i| format!("{stem}-{i:0width$}-of-{count:0width$}.gguf"))
            .collect(),
    )
}

/// All files holding this model's tensor data: `[path]` for a single-file
/// model, or every `-NNNNN-of-NNNNN.gguf` sibling (in shard order) for a
/// split. Split shards are each standalone GGUFs with their own header +
/// tensor-info + data section, so every downstream reader parses them
/// per-file. Returns `[path]` (never guesses) when any sibling is missing.
pub fn shard_files(model_path: &Path) -> Vec<std::path::PathBuf> {
    let single = vec![model_path.to_path_buf()];
    let Some(name) = model_path.file_name().and_then(|n| n.to_str()) else {
        return single;
    };
    let Some(dir) = model_path.parent() else {
        return single;
    };
    let Some(names) = split_shard_names(name) else {
        return single;
    };
    let mut files = Vec::with_capacity(names.len());
    for shard_name in names {
        let shard = dir.join(shard_name);
        if !shard.is_file() {
            return single; // a shard missing → don't guess
        }
        files.push(shard);
    }
    files
}

#[cfg(test)]
mod tests;
