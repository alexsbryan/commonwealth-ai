// SPDX-License-Identifier: AGPL-3.0-or-later
// Reverse a string.
//
// Returns the input string with its characters in reverse order.
// Multi-byte UTF-8 characters are reversed as whole code points.

pub fn reverse_string(s: &str) -> String {
    s.chars().rev().collect()
}
