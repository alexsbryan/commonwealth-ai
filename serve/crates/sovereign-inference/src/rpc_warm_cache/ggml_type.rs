// SPDX-License-Identifier: AGPL-3.0-or-later
//! ggml's per-type storage sizes, so a tensor's byte length is computed
//! without linking ggml. Values are ggml's `type_traits` (`ggml.c`) and the
//! block structs of `ggml-common.h` at the vendored llama.cpp; the golden test
//! `ggml_type_size_golden_tests` holds every row against `ggml_row_size`.

/// `(elements per block, bytes per block)` for each ggml type id a GGUF can
/// carry. Ids ggml removed (4, 5, 31-33, 36-38) are absent, as is anything
/// past `GGML_TYPE_COUNT` (43).
pub(crate) const GGML_TYPE_SIZES: &[(u32, u64, u64)] = &[
    (0, 1, 4),      // F32
    (1, 1, 2),      // F16
    (2, 32, 18),    // Q4_0
    (3, 32, 20),    // Q4_1
    (6, 32, 22),    // Q5_0
    (7, 32, 24),    // Q5_1
    (8, 32, 34),    // Q8_0
    (9, 32, 36),    // Q8_1
    (10, 256, 84),  // Q2_K
    (11, 256, 110), // Q3_K
    (12, 256, 144), // Q4_K
    (13, 256, 176), // Q5_K
    (14, 256, 210), // Q6_K
    (15, 256, 292), // Q8_K
    (16, 256, 66),  // IQ2_XXS
    (17, 256, 74),  // IQ2_XS
    (18, 256, 98),  // IQ3_XXS
    (19, 256, 50),  // IQ1_S
    (20, 32, 18),   // IQ4_NL
    (21, 256, 110), // IQ3_S
    (22, 256, 82),  // IQ2_S
    (23, 256, 136), // IQ4_XS
    (24, 1, 1),     // I8
    (25, 1, 2),     // I16
    (26, 1, 4),     // I32
    (27, 1, 8),     // I64
    (28, 1, 8),     // F64
    (29, 256, 56),  // IQ1_M
    (30, 1, 2),     // BF16
    (34, 256, 54),  // TQ1_0
    (35, 256, 66),  // TQ2_0
    (39, 32, 17),   // MXFP4
    (40, 64, 36),   // NVFP4
    (41, 128, 18),  // Q1_0
    (42, 64, 18),   // Q2_0
];

/// Bytes in one row of `ne` elements of ggml type `ty` — `ggml_row_size`'s
/// `type_size * ne / blck_size`. `None` for a type id the table does not know.
pub(crate) fn ggml_row_bytes(ty: u32, ne: u64) -> Option<u64> {
    GGML_TYPE_SIZES
        .iter()
        .find(|(id, _, _)| *id == ty)
        .map(|&(_, blck, size)| size * ne / blck)
}
