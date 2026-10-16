// SPDX-License-Identifier: AGPL-3.0-or-later
//! Numbers written in words, read to their values: the one reader the deep
//! research figure tokens (`deep_research::normalize_word_figures`) and the
//! verify specifics finder (`grounding::specifics`) share.
//!
//! A word is a cardinal ("forty", "thousand") or an ordinal ("fortieth"),
//! matched case-insensitively and whole-word; a hyphen compound counts only
//! when every part is one ("forty-eight", "twenty-first"). A phrase is a run
//! of them with an "and" between two ("eight hundred and fifty thousand"),
//! composed the standard way: a word under a hundred adds, "hundred"
//! multiplies what precedes it, and "thousand", "million", "billion" close a
//! group. A scale word with nothing before it counts one of itself
//! ("hundred" is 100). Deterministic, no model.

/// One word's value, and what kind of number word it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NumberWord {
    pub value: u64,
    /// Hundred, thousand, million, billion: multiplicative.
    pub scale: bool,
    /// "first", "twentieth": an order, not a count.
    pub ordinal: bool,
}

/// The value of one number word. The vocabulary is closed over the
/// adversarial generator's NUMBER_WORDS/ORDINAL_WORDS
/// (`sovereign-eval/src/flywheel/generators/adversarial.rs`) with the
/// units/teens/tens ranges they imply, plus the scale words to a billion.
/// Ordinals carry their cardinal's value ("twentieth" is 20).
pub(crate) fn number_word_value(w: &str) -> Option<NumberWord> {
    let lower = w.to_ascii_lowercase();
    let (value, scale, ordinal) = match lower.as_str() {
        "one" => (1, false, false),
        "first" => (1, false, true),
        "two" => (2, false, false),
        "second" => (2, false, true),
        "three" => (3, false, false),
        "third" => (3, false, true),
        "four" => (4, false, false),
        "fourth" => (4, false, true),
        "five" => (5, false, false),
        "fifth" => (5, false, true),
        "six" => (6, false, false),
        "seven" => (7, false, false),
        "eight" => (8, false, false),
        "eighth" => (8, false, true),
        "nine" => (9, false, false),
        "ninth" => (9, false, true),
        "ten" => (10, false, false),
        "eleven" => (11, false, false),
        "twelve" => (12, false, false),
        "twelfth" => (12, false, true),
        "thirteen" => (13, false, false),
        "fourteen" => (14, false, false),
        "fifteen" => (15, false, false),
        "sixteen" => (16, false, false),
        "seventeen" => (17, false, false),
        "eighteen" => (18, false, false),
        "nineteen" => (19, false, false),
        "twenty" => (20, false, false),
        "thirty" => (30, false, false),
        "forty" => (40, false, false),
        "fifty" => (50, false, false),
        "sixty" => (60, false, false),
        "seventy" => (70, false, false),
        "eighty" => (80, false, false),
        "ninety" => (90, false, false),
        "hundred" => (100, true, false),
        "thousand" => (1_000, true, false),
        "million" => (1_000_000, true, false),
        "billion" => (1_000_000_000, true, false),
        other => return regular_ordinal(other),
    };
    Some(NumberWord {
        value,
        scale,
        ordinal,
    })
}

/// "sixth", "tenth", "fourteenth", "twentieth": a cardinal plus "th", or a
/// tens word with its "y" become "ieth".
fn regular_ordinal(lower: &str) -> Option<NumberWord> {
    let cardinal = match lower.strip_suffix("ieth") {
        Some(stem) => format!("{stem}y"),
        None => lower.strip_suffix("th")?.to_string(),
    };
    let w = number_word_value(&cardinal).filter(|w| !w.ordinal && !w.scale)?;
    Some(NumberWord { ordinal: true, ..w })
}

/// Whether `w` is a number word or a hyphen compound of them
/// ("fifty-eight", "twenty-first"); "state-of-the-art" is not.
pub(crate) fn is_number_word(w: &str) -> bool {
    w.split('-').all(|part| number_word_value(part).is_some())
}

/// A number phrase read from the front of a run of words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NumberPhrase {
    /// How many of the words it took, its "and"s included.
    pub words: usize,
    pub value: u64,
    /// Whether any word of it is an ordinal ("twenty-first").
    pub ordinal: bool,
    /// Whether any word of it is a scale word ("a hundred" reads only then).
    pub scaled: bool,
}

/// The longest number phrase opening `words` (each a bare word, its
/// punctuation already cut), or `None` when the first is no number word or
/// the value overflows. An "and" belongs to the phrase only between two
/// number words: "twenty and more" is "twenty".
pub(crate) fn number_phrase<S: AsRef<str>>(words: &[S]) -> Option<NumberPhrase> {
    let mut parts: Vec<NumberWord> = Vec::new();
    let mut taken = 0;
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_ref();
        if is_number_word(w) {
            parts.extend(w.split('-').filter_map(number_word_value));
            i += 1;
            taken = i;
        } else if w.eq_ignore_ascii_case("and")
            && !parts.is_empty()
            && words.get(i + 1).is_some_and(|n| is_number_word(n.as_ref()))
        {
            i += 1;
        } else {
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    let (mut total, mut group) = (0u64, 0u64);
    for p in &parts {
        if !p.scale {
            group = group.checked_add(p.value)?;
        } else if p.value == 100 {
            group = group.max(1).checked_mul(100)?;
        } else {
            total = total.checked_add(group.max(1).checked_mul(p.value)?)?;
            group = 0;
        }
    }
    Some(NumberPhrase {
        words: taken,
        value: total.checked_add(group)?,
        ordinal: parts.iter().any(|p| p.ordinal),
        scaled: parts.iter().any(|p| p.scale),
    })
}

/// Whether a phrase opening with `first` right after the word `prev` is
/// the magnitude of the digits before it ("$32 billion", "4.5 million"),
/// not a number of its own.
pub(crate) fn magnitude_of_digits(prev: Option<&str>, first: &str) -> bool {
    prev.is_some_and(|p| p.chars().any(|c| c.is_ascii_digit()))
        && first
            .split('-')
            .next()
            .and_then(number_word_value)
            .is_some_and(|w| w.scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> Option<(usize, u64, bool)> {
        let words: Vec<&str> = s.split_whitespace().collect();
        number_phrase(&words).map(|p| (p.words, p.value, p.ordinal))
    }

    #[test]
    fn phrases_compose_by_scale() {
        assert_eq!(read("forty-eight thousand pins"), Some((2, 48_000, false)));
        assert_eq!(read("hundred years"), Some((1, 100, false)));
        assert_eq!(
            read("eight hundred and fifty thousand people"),
            Some((5, 850_000, false))
        );
        assert_eq!(read("one hundred thousand"), Some((3, 100_000, false)));
        assert_eq!(read("two thousand five hundred"), Some((4, 2_500, false)));
        assert_eq!(
            read("one million two hundred thousand"),
            Some((5, 1_200_000, false))
        );
        assert_eq!(read("twenty and more"), Some((1, 20, false)));
        assert_eq!(read("twenty-first year"), Some((1, 21, true)));
        assert_eq!(read("tenth part"), Some((1, 10, true)));
        assert_eq!(read("fortieth"), Some((1, 40, true)));
        assert_eq!(read("state-of-the-art"), None);
        assert_eq!(read("and ten"), None);
        assert!(magnitude_of_digits(Some("$32"), "billion"));
        assert!(magnitude_of_digits(Some("4.5"), "thousand"));
        assert!(!magnitude_of_digits(Some("4.5"), "twenty"));
        assert!(!magnitude_of_digits(Some("about"), "thousand"));
    }
}
