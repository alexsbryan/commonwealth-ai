// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Rust ggml type-size table against the linked ggml. Lives beside
//! `crate::llama` and goes with it: once nothing links ggml there is no
//! oracle left to hold the table to.

use crate::rpc_warm_cache::ggml_type::{ggml_row_bytes, GGML_TYPE_SIZES};

#[test]
fn every_type_row_size_matches_ggml() {
    for &(ty, blck, _) in GGML_TYPE_SIZES {
        for blocks in [1u64, 2, 7, 1024] {
            let ne = blck * blocks;
            // SAFETY: a pure size calculation over a type id ggml defines.
            let ffi = unsafe {
                crate::llama::sys::ggml_row_size(ty as crate::llama::sys::ggml_type, ne as i64)
            } as u64;
            assert_eq!(ggml_row_bytes(ty, ne), Some(ffi), "ggml type {ty}, ne {ne}");
        }
    }
}

#[test]
fn the_table_covers_every_type_ggml_counts() {
    // Every id below GGML_TYPE_COUNT that ggml still sizes (blck_size > 0) is
    // in the table, so a type ggml adds cannot reach the planner unsized.
    let count = crate::llama::sys::GGML_TYPE_COUNT as u32;
    for ty in 0..count {
        // SAFETY: ggml_blck_size reads the static type_traits row for `ty`.
        let blck = unsafe { crate::llama::sys::ggml_blck_size(ty as crate::llama::sys::ggml_type) };
        let known = GGML_TYPE_SIZES.iter().any(|(id, _, _)| *id == ty);
        assert_eq!(blck > 0, known, "ggml type {ty}: blck_size {blck}");
    }
    assert_eq!(ggml_row_bytes(count, 256), None);
}
