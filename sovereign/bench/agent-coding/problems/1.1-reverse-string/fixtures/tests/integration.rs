//! Held-out integration tests for problem 1.1 — `reverse_string`.
//!
//! The witness installs this file at `tests/integration.rs` in the
//! agent's workdir AFTER the agent has produced its `src/lib.rs`,
//! then runs `cargo test --quiet --test integration`. The agent has
//! no view of these expectations beyond the prompt's described
//! behaviour.

use reverse_string::reverse_string;

#[test]
fn empty_returns_empty() {
    assert_eq!(reverse_string(""), "");
}

#[test]
fn single_char_passes_through() {
    assert_eq!(reverse_string("x"), "x");
}

#[test]
fn ascii_word_reverses() {
    assert_eq!(reverse_string("hello"), "olleh");
}

#[test]
fn ascii_palindrome_unchanged() {
    assert_eq!(reverse_string("racecar"), "racecar");
}

#[test]
fn ascii_with_punctuation_reverses() {
    assert_eq!(reverse_string("hello, world!"), "!dlrow ,olleh");
}

#[test]
fn whitespace_reverses() {
    assert_eq!(reverse_string("a b c"), "c b a");
}

#[test]
fn multi_byte_utf8_keeps_code_points_intact() {
    // "héllo" — `é` is two bytes (0xC3 0xA9). A naive byte-reverse
    // would emit 0xA9 0xC3 first and yield invalid UTF-8. Scalar
    // reversal yields valid UTF-8 with the `é` near the end.
    let input = "héllo";
    let result = reverse_string(input);
    assert_eq!(result, "olléh");
    // Sanity: the result must remain valid UTF-8.
    assert!(std::str::from_utf8(result.as_bytes()).is_ok());
}

#[test]
fn longer_multi_byte_word() {
    assert_eq!(reverse_string("café"), "éfac");
}

#[test]
fn cjk_reverses_by_scalar() {
    // Each kanji is one Unicode scalar (3 bytes in UTF-8).
    assert_eq!(reverse_string("日本語"), "語本日");
}

#[test]
fn emoji_with_no_combining_marks_reverses() {
    // 🍎 (U+1F34E) and 🍌 (U+1F34C) are each one scalar (4 bytes).
    assert_eq!(reverse_string("🍎🍌"), "🍌🍎");
}

#[test]
fn mixed_ascii_and_multibyte() {
    assert_eq!(reverse_string("a→b"), "b→a");
}

#[test]
fn long_string_reverses() {
    let input: String = ('a'..='z').collect();
    let expected: String = ('a'..='z').rev().collect();
    assert_eq!(reverse_string(&input), expected);
}
