// SPDX-License-Identifier: AGPL-3.0-or-later
//! Smoke tests visible to the agent during development.
//!
//! These three cases are a SUBSET of the held-out grading suite. The
//! grader copies its full 12-fixture suite over this file (same
//! path, same name) AFTER the agent exits — so this file is your
//! iteration substrate, not the final scoring oracle. Passing all
//! three smoke tests is necessary but not sufficient.

use reverse_string::reverse_string;

#[test]
fn smoke_empty_returns_empty() {
    assert_eq!(reverse_string(""), "");
}

#[test]
fn smoke_ascii_word() {
    assert_eq!(reverse_string("hello"), "olleh");
}

#[test]
fn smoke_multibyte_scalars_reverse_whole() {
    // Multi-byte UTF-8 characters must reverse as whole scalar
    // values, never byte-by-byte.
    assert_eq!(reverse_string("héllo"), "olléh");
}
