// SPDX-License-Identifier: AGPL-3.0-or-later
//! The name fold: case + trim + Russian-Cyrillic transliteration + Latin
//! combining-diacritic strip.
//!
//! Moved out of `corpus-engine`'s HOST `enrichment/atlas/resolution.rs` by
//! domains `dm-understanding-pure-1` (ralph/DECISIONS.md): pure files
//! (`atlas_traversal::classifier`, `atlas::cross_corpus`,
//! `atlas::resolution_ontology`) name it, and it is arithmetic — no IO, no
//! engine — so it belongs to the pure tier. `resolution.rs` re-exports it at
//! the historical `crate::enrichment::atlas::fold` path.
//!
//! The name index is keyed on this form so lookups are forgiving of:
//!
//! - Case and surrounding whitespace.
//! - Russian-novel LLM output that mixes Cyrillic mid-word
//!   (`Karamазов` ↔ `Karamazov`) — collapsed by `transliterate_cyrillic`.
//! - Latin diacritic drift from models that over-decorate transliterations
//!   (`Karámazov` ↔ `Karamazov`, `Fyódor` ↔ `Fyodor`, `Miüsov` ↔ `Miusov`) —
//!   collapsed by NFD decomposition followed by dropping Unicode combining
//!   marks.
//!
//! The NFD step decomposes a precomposed `á` into `a` + U+0301 (combining
//! acute); we then filter the marks out, leaving plain `a`. This makes fold
//! idempotent under diacritic perturbation — the model can emit any mixture
//! of decorations and the index still finds the entity.
//!
//! Scope: Russian (and passthrough Ukrainian) Cyrillic; Latin diacritics
//! across the full Unicode combining-mark block. Adding Serbian, Greek, or
//! Arabic scripts is cheap but untested; do it when a corpus requires it.
pub fn fold(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    transliterate_cyrillic(&s.trim().to_lowercase())
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect()
}

/// Best-effort lower-case Russian Cyrillic → Latin transliteration.
/// Passes every non-Cyrillic char through unchanged — so an already-Latin
/// string round-trips byte-for-byte. Chosen to match the transliteration the
/// LLM itself produces when asked for an English form (Karamazov, Zosima,
/// Alyosha).
fn transliterate_cyrillic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let repl: &str = match c {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' => "",
            'ы' => "y",
            'ь' => "",
            'э' => "e",
            'ю' => "yu",
            'я' => "ya",
            // Ukrainian additions — cheap to include; exact Russian
            // texts will never hit these branches.
            'є' => "ye",
            'і' => "i",
            'ї' => "yi",
            'ґ' => "g",
            // Any other char passes through.
            other => {
                out.push(other);
                continue;
            }
        };
        out.push_str(repl);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{fold, transliterate_cyrillic};

    #[test]
    fn fold_strips_latin_combining_diacritics_for_folded_lookup() {
        // Observed in the Landing 2 smoke test: the model emits
        // decorated Latin forms like `Karámazov`, `Fyódor Pávlovič`,
        // `Miüsov`. NFD decomposes the precomposed diacritic char
        // and we drop the combining mark, leaving plain Latin.
        // This makes fold idempotent regardless of which mixture of
        // diacritics the model chose this call.
        assert_eq!(fold("Karámazov"), "karamazov");
        assert_eq!(fold("Fyódor Pávlovič"), "fyodor pavlovic");
        assert_eq!(fold("Miüsov"), "miusov");
        // Mixed Cyrillic + Latin-diacritic case (realistic drift):
        // Cyrillic `а` transliterates to Latin `a`, the á loses
        // its acute. Final form matches the clean canonical.
        assert_eq!(fold("Karámázов"), "karamazov");
        // Already-plain input passes through byte-for-byte.
        assert_eq!(fold("Karamazov"), "karamazov");
    }

    #[test]
    fn transliterate_cyrillic_maps_russian_to_english_form() {
        // The canonical Brothers Karamazov cases that the raw
        // resolver misses: mid-word Cyrillic chars in a Latin
        // transliteration should fold to the same form.
        assert_eq!(transliterate_cyrillic("karamазов"), "karamazov");
        assert_eq!(transliterate_cyrillic("adelаida"), "adelaida");
        assert_eq!(transliterate_cyrillic("mityа"), "mitya");
        // Already-Latin strings pass through unchanged.
        assert_eq!(transliterate_cyrillic("karamazov"), "karamazov");
        // Pure Russian spelling transliterates to the English form.
        assert_eq!(transliterate_cyrillic("карамазов"), "karamazov");
        // Non-Cyrillic passthrough preserves spaces / punctuation.
        assert_eq!(
            transliterate_cyrillic("fyodor pavlovich karamazov"),
            "fyodor pavlovich karamazov"
        );
    }
}
