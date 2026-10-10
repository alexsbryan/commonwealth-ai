// SPDX-License-Identifier: AGPL-3.0-or-later
//! The decode-time allow-lists as languages: which bytes open an entry, how
//! an allowed id is written, and which bytes may end one. Defined once so the
//! engine's tries (`sovereign-inference`'s `url_constraint` and
//! `evidence_id_constraint`) and the grammar a remote host is sent
//! ([`llguidance_lark`]) enforce the same thing.
//!
//! Outside an entry any text is allowed, as long as it never contains a
//! marker. Once a marker appears, the bytes must spell an allowed entry and
//! then end, or be followed by one of the language's terminators.

/// One allow-list's language.
pub struct AllowlistLanguage {
    /// Byte strings that open an entry, checked in this order.
    pub markers: &'static [&'static [u8]],
    /// Written before an allowed id to make its entry: an evidence handle
    /// is cited in brackets, a URL as itself.
    pub entry_prefix: &'static str,
    /// Bytes that may follow a complete entry.
    pub terminators: &'static [u8],
}

impl AllowlistLanguage {
    /// Whether `b` may follow a complete entry.
    pub fn is_terminator(&self, b: u8) -> bool {
        self.terminators.contains(&b)
    }

    /// The entry an allowed `id` is written as.
    pub fn entry(&self, id: &str) -> String {
        format!("{}{id}", self.entry_prefix)
    }

    /// The entries in `text` this language rejects for `allowed`: at each
    /// marker an allowed entry must follow, then a terminator or the end of
    /// the text. A rejected entry is returned from its marker to the next
    /// whitespace. This is how an output is checked when its mask ran where
    /// it cannot be observed.
    pub fn entries_outside<'t>(&self, text: &'t str, allowed: &[String]) -> Vec<&'t str> {
        let bytes = text.as_bytes();
        let entries: Vec<String> = allowed.iter().map(|id| self.entry(id)).collect();
        let mut outside = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if !self.markers.iter().any(|m| bytes[at..].starts_with(m)) {
                at += 1;
                continue;
            }
            let accepted = entries
                .iter()
                .filter(|e| bytes[at..].starts_with(e.as_bytes()))
                .map(|e| at + e.len())
                .find(|&end| end == bytes.len() || self.is_terminator(bytes[end]));
            at = match accepted {
                Some(end) => end,
                None => {
                    let end = (at..bytes.len())
                        .find(|&i| bytes[i].is_ascii_whitespace())
                        .unwrap_or(bytes.len());
                    outside.push(&text[at..end]);
                    end
                }
            };
        }
        outside
    }
}

/// `url_allowlist`: cited URLs.
pub const URL: AllowlistLanguage = AllowlistLanguage {
    markers: &[b"https://", b"http://"],
    entry_prefix: "",
    terminators: b" \t\n\r,.<>()[]\"'?!;:",
};

/// `evidence_id_allowlist`: `ev-Tn-NNNN` handles. The bracket is part of the
/// marker, so a bare `ev-T` in prose does not engage it.
pub const EVIDENCE_ID: AllowlistLanguage = AllowlistLanguage {
    markers: &[b"[ev-T"],
    entry_prefix: "[",
    terminators: b"] \t\n\r,.()<>\"'?!;:",
};

/// The allow-lists as one grammar in llguidance's Lark (a llama-server built
/// with `LLAMA_LLGUIDANCE` takes it as `grammar`). `None` when every list is
/// empty, as the engine applies no constraint for an empty list.
///
/// Each list is one regular token, and several lists are their intersection
/// (`&`): the engine runs one independent mask per list, and `[` both ends a
/// URL and opens an evidence handle, which one combined loop would let
/// through unchecked.
pub fn llguidance_lark(lists: &[(&AllowlistLanguage, &[String])]) -> Option<String> {
    let mut defs = String::new();
    let mut names = Vec::new();
    for (i, (lang, ids)) in lists.iter().enumerate() {
        if ids.is_empty() {
            continue;
        }
        let markers = lang
            .markers
            .iter()
            .map(|m| m.iter().map(|&b| regex_byte(b)).collect::<String>())
            .collect::<Vec<_>>()
            .join("|");
        let entries = ids
            .iter()
            .map(|id| serde_json::Value::String(lang.entry(id)).to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        let terms: String = lang.terminators.iter().map(|&b| regex_byte(b)).collect();
        defs.push_str(&format!(
            "L{i}: F{i} (A{i} T{i} F{i})* A{i}?\n\
             F{i}: /(?s:.*)/ & ~/(?s:.*)(?:{markers})(?s:.*)/\n\
             A{i}: {entries}\n\
             T{i}: /[{terms}]/\n"
        ));
        names.push(format!("L{i}"));
    }
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "%llguidance {{}}\nstart: OUT\nOUT: {}\n{defs}",
        names.join(" & ")
    ))
}

/// `b` as a regex atom that means itself inside or outside a class, and
/// cannot close the `/.../` around it.
fn regex_byte(b: u8) -> String {
    if b.is_ascii_alphanumeric() {
        (b as char).to_string()
    } else {
        format!("\\x{b:02x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_allowed_entry_followed_by_a_terminator_or_the_end_is_inside() {
        let urls = list(&["https://a.org/x", "https://a.org"]);
        assert!(URL
            .entries_outside("See https://a.org/x, and https://a.org.", &urls)
            .is_empty());
        assert!(URL.entries_outside("ends on https://a.org/x", &urls).is_empty());
        let ids = list(&["ev-T1-0002"]);
        assert!(EVIDENCE_ID
            .entries_outside("as shown [ev-T1-0002]; bare ev-T9 is prose", &ids)
            .is_empty());
    }

    #[test]
    fn anything_else_after_a_marker_is_outside() {
        let urls = list(&["https://a.org"]);
        assert_eq!(
            URL.entries_outside("https://a.org/y and http://b.net/z", &urls),
            ["https://a.org/y", "http://b.net/z"]
        );
        assert_eq!(URL.entries_outside("cut off at https://a.o", &urls), ["https://a.o"]);
        assert_eq!(
            EVIDENCE_ID.entries_outside("[ev-T1-0003] and [ev-T1-000", &list(&["ev-T1-0002"])),
            ["[ev-T1-0003]", "[ev-T1-000"]
        );
    }
}
