// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one L2 normalisation (ARCH §10.6).
//!
//! Used by the question-kind centroid classifier here and by corpus-engine's
//! column-aware header classifier, which re-imports it from this leaf.

/// Normalise `v` in place to unit L2 length. A zero vector is left as-is —
/// "no direction" is a real state, not a division error.
pub fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}
