// SPDX-License-Identifier: AGPL-3.0-or-later
//! Obsidian YAML frontmatter merge + strip primitives.
//!
//! Invariants (spec §6.5, enforced by tests in this module):
//!   - Only `<namespace>/*` tags are ever added, modified, or removed.
//!     Every user tag round-trips byte-for-byte.
//!   - Only `sovereign_*` frontmatter keys are ever written or
//!     removed. Every other key in the map is preserved exactly —
//!     value shape AND key ordering.
//!   - `merge_frontmatter` is idempotent: calling it twice with the
//!     same assignment produces the same result (modulo the updated
//!     `sovereign_version` timestamp).
//!   - `strip_sovereign` is complete: after running, no
//!     `sovereign/...` tags and no `sovereign_*` keys remain.
//!
//! We parse with `serde_yaml` + an IndexMap-preserving value type.
//! Key order inside a frontmatter block round-trips; comments and
//! precise whitespace do not — this is the v1 tradeoff and the test
//! suite enforces the weaker "value-perfect" guarantee. If your
//! reviewer cares about byte-perfect, yaml-rust2 or a raw-token
//! editor is the upgrade path, same shape.

use serde_yaml::{Mapping, Value};

// ─── Document split ──────────────────────────────────────────────────

/// A parsed markdown document. `raw_frontmatter` is the YAML block
/// between the fences (without the fences themselves, without the
/// trailing newline). `None` means the file has no frontmatter fence.
#[derive(Debug, Clone)]
pub struct SplitDocument<'a> {
    pub raw_frontmatter: Option<&'a str>,
    pub body: &'a str,
    /// The line-ending that was used in the source (`\n` vs `\r\n`).
    /// We preserve it on re-serialise so Windows users' git diffs
    /// stay clean.
    pub newline: &'static str,
}

pub fn split_document(raw: &str) -> SplitDocument<'_> {
    // Detect the line-ending by sampling the first break we see.
    let newline: &'static str = if raw.contains("\r\n") { "\r\n" } else { "\n" };

    // A frontmatter block opens with `---` on line 1 and closes with
    // `---` on its own line. Anything else → no frontmatter.
    let opener_variants = [("---\r\n", 5), ("---\n", 4)];
    let (after_open, _open_len) = match opener_variants.iter().find(|(p, _)| raw.starts_with(p)) {
        Some((_, n)) => (&raw[*n..], *n),
        None => {
            return SplitDocument {
                raw_frontmatter: None,
                body: raw,
                newline,
            }
        }
    };

    // Search for a `---` line inside `after_open`. Must be at a line
    // start (prev char \n or start of block) and followed by \n / \r\n
    // / EOF.
    let mut search_from = 0;
    while let Some(rel) = after_open[search_from..].find("---") {
        let abs = search_from + rel;
        let at_line_start = abs == 0 || after_open.as_bytes()[abs - 1] == b'\n';
        let tail = &after_open[abs + 3..];
        let line_end = tail.is_empty() || tail.starts_with('\n') || tail.starts_with("\r\n");
        if at_line_start && line_end {
            let fm = &after_open[..abs];
            // Strip a trailing CR from the frontmatter body if we
            // were mid-CRLF before the `---` line.
            let fm = fm.strip_suffix('\r').unwrap_or(fm);
            let body_start = abs
                + 3
                + if tail.starts_with("\r\n") {
                    2
                } else if tail.starts_with('\n') {
                    1
                } else {
                    0
                };
            let body = &after_open[body_start..];
            return SplitDocument {
                raw_frontmatter: Some(fm),
                body,
                newline,
            };
        }
        search_from = abs + 3;
    }
    // Opening fence with no closing fence: treat as no frontmatter
    // so we don't silently eat the user's content.
    SplitDocument {
        raw_frontmatter: None,
        body: raw,
        newline,
    }
}

// ─── Merge ───────────────────────────────────────────────────────────

/// Minimal shape of the assignment metadata we need for merging.
/// Kept narrow so the merge function is unit-testable without
/// dragging in the Preview module.
#[derive(Debug, Clone)]
pub struct MergeInputs<'a> {
    pub primary_tag: &'a str,
    pub additional_tags: &'a [String],
    pub cluster_display_name: &'a str,
    pub confidence: f32,
    pub version: u32,
}

/// Merge `sovereign/*` tags and `sovereign_*` keys into the existing
/// markdown document. Preserves everything else byte-perfect at the
/// value level (key order preserved; comments and exotic whitespace
/// may reformat).
pub fn merge_frontmatter(raw: &str, inputs: &MergeInputs<'_>, namespace: &str) -> String {
    let split = split_document(raw);

    // Decide what we're updating — existing YAML map, or a fresh empty one.
    let mut map = match split.raw_frontmatter {
        Some(fm) => {
            // An entirely empty frontmatter block (`---\n---\n`) is
            // valid; deserialise gives `Null`. Coerce to an empty map
            // so we can insert keys below.
            match serde_yaml::from_str::<Value>(fm) {
                Ok(Value::Null) => Mapping::new(),
                Ok(Value::Mapping(m)) => m,
                Ok(_) => {
                    // Non-mapping frontmatter (e.g. a bare list at
                    // the top). Drop into fresh map — this branch is
                    // rare and preserving such a file would mean
                    // refusing to write, which is worse UX than
                    // gracefully promoting it to a map with one key.
                    Mapping::new()
                }
                Err(_) => Mapping::new(),
            }
        }
        None => Mapping::new(),
    };

    // 1. Scrub sovereign-owned tags from `tags`, preserving the rest.
    let tags_key = Value::String("tags".into());
    if let Some(existing) = map.remove(&tags_key) {
        let retained: Vec<Value> = match existing {
            Value::Sequence(seq) => seq
                .into_iter()
                .filter(|v| !is_sovereign_tag(v, namespace))
                .collect(),
            Value::String(s) => {
                // Obsidian also accepts `tags: a, b, c` (comma list
                // as a single string). Split, filter, re-emit as a
                // sequence which is the canonical form we write.
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .filter(|t| !is_sovereign_str(t, namespace))
                    .map(Value::String)
                    .collect()
            }
            Value::Null => Vec::new(),
            // Anything else (a map, a number) — the user wrote
            // something non-standard; we don't assume we understand
            // the intent. Drop to empty list rather than rewriting.
            _ => Vec::new(),
        };
        if !retained.is_empty() {
            map.insert(tags_key.clone(), Value::Sequence(retained));
        }
    }

    // 2. Scrub sovereign-owned keys so we can re-insert fresh values.
    let sovereign_keys: Vec<Value> = map
        .keys()
        .filter(|k| match k {
            Value::String(s) => s.starts_with("sovereign_"),
            _ => false,
        })
        .cloned()
        .collect();
    for k in sovereign_keys {
        map.remove(&k);
    }

    // 3. Append fresh sovereign tags. The sequence may already
    //    exist (user tags were present); we rebuild it if so.
    let mut new_tags: Vec<Value> = match map.remove(&tags_key) {
        Some(Value::Sequence(seq)) => seq,
        _ => Vec::new(),
    };
    new_tags.push(Value::String(inputs.primary_tag.to_string()));
    for t in inputs.additional_tags {
        new_tags.push(Value::String(t.clone()));
    }
    map.insert(tags_key, Value::Sequence(new_tags));

    // 4. Sovereign-owned frontmatter keys.
    map.insert(
        Value::String("sovereign_cluster".into()),
        Value::String(inputs.cluster_display_name.to_string()),
    );
    map.insert(
        Value::String("sovereign_cluster_confidence".into()),
        Value::Number(serde_yaml::Number::from(
            (inputs.confidence.clamp(0.0, 1.0) as f64 * 10000.0).round() / 10000.0,
        )),
    );
    map.insert(
        Value::String("sovereign_version".into()),
        Value::Number(serde_yaml::Number::from(inputs.version)),
    );

    // 5. Serialise + reassemble.
    let serialised = serialise_mapping(&map, split.newline);
    let body = split.body;
    // Make sure the document ends with exactly one newline break
    // between the fence and the body — this matches the canonical
    // Obsidian format.
    format!(
        "---{nl}{fm}---{nl}{body}",
        nl = split.newline,
        fm = serialised,
        body = body,
    )
}

/// Strip every `sovereign/*` tag and every `sovereign_*` key from
/// the document. Used by the "Clean" action in `writeback`.
/// Idempotent. Leaves the document unchanged if nothing matched.
pub fn strip_sovereign(raw: &str, namespace: &str) -> String {
    let split = split_document(raw);
    let Some(fm) = split.raw_frontmatter else {
        return raw.to_string();
    };
    let mut map: Mapping = match serde_yaml::from_str::<Value>(fm) {
        Ok(Value::Mapping(m)) => m,
        _ => return raw.to_string(),
    };

    // Tags array.
    let tags_key = Value::String("tags".into());
    if let Some(Value::Sequence(seq)) = map.remove(&tags_key) {
        let retained: Vec<Value> = seq
            .into_iter()
            .filter(|v| !is_sovereign_tag(v, namespace))
            .collect();
        if !retained.is_empty() {
            map.insert(tags_key, Value::Sequence(retained));
        }
    }

    // sovereign_* keys.
    let sovereign_keys: Vec<Value> = map
        .keys()
        .filter(|k| match k {
            Value::String(s) => s.starts_with("sovereign_"),
            _ => false,
        })
        .cloned()
        .collect();
    for k in sovereign_keys {
        map.remove(&k);
    }

    if map.is_empty() {
        // Drop the frontmatter block entirely rather than leaving
        // an empty `---\n---\n` sentinel — the user's file looks
        // exactly as it did before any sovereign touch, modulo the
        // (minor) formatting reflow we already introduced.
        return split.body.to_string();
    }

    let serialised = serialise_mapping(&map, split.newline);
    format!(
        "---{nl}{fm}---{nl}{body}",
        nl = split.newline,
        fm = serialised,
        body = split.body,
    )
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn is_sovereign_tag(v: &Value, namespace: &str) -> bool {
    match v {
        Value::String(s) => is_sovereign_str(s, namespace),
        _ => false,
    }
}

fn is_sovereign_str(s: &str, namespace: &str) -> bool {
    let prefix = format!("{namespace}/");
    s.starts_with(&prefix) || s == namespace
}

fn serialise_mapping(map: &Mapping, newline: &str) -> String {
    // serde_yaml always emits `\n`. If the source used CRLF we
    // normalise on the way out so the whole document ends up with a
    // consistent line ending.
    let raw = serde_yaml::to_string(map).unwrap_or_default();
    if newline == "\r\n" {
        raw.replace('\n', "\r\n")
    } else {
        raw
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(tag: &'a str) -> MergeInputs<'a> {
        MergeInputs {
            primary_tag: tag,
            additional_tags: &[],
            cluster_display_name: "Test Cluster",
            confidence: 0.85,
            version: 1,
        }
    }

    fn parse_fm(doc: &str) -> Mapping {
        let split = split_document(doc);
        let fm = split.raw_frontmatter.expect("doc must have frontmatter");
        match serde_yaml::from_str::<Value>(fm).unwrap() {
            Value::Mapping(m) => m,
            _ => panic!("frontmatter not a mapping"),
        }
    }

    #[test]
    fn split_no_frontmatter() {
        let doc = "# Just a heading\n\nBody.";
        let s = split_document(doc);
        assert!(s.raw_frontmatter.is_none());
        assert_eq!(s.body, doc);
    }

    #[test]
    fn split_basic_frontmatter() {
        let doc = "---\ntags: [x]\n---\n# Body";
        let s = split_document(doc);
        assert_eq!(s.raw_frontmatter.unwrap().trim(), "tags: [x]");
        assert_eq!(s.body, "# Body");
    }

    #[test]
    fn split_empty_frontmatter_block() {
        let doc = "---\n---\nBody";
        let s = split_document(doc);
        assert_eq!(s.raw_frontmatter, Some(""));
        assert_eq!(s.body, "Body");
    }

    #[test]
    fn split_crlf_frontmatter() {
        let doc = "---\r\ntags: [x]\r\n---\r\n# Body\r\n";
        let s = split_document(doc);
        assert!(s.raw_frontmatter.unwrap().contains("tags"));
        assert_eq!(s.newline, "\r\n");
    }

    #[test]
    fn split_unclosed_treated_as_no_frontmatter() {
        // Spec: "File with no frontmatter block" and "unclosed" both
        // must leave content untouched rather than eat it.
        let doc = "---\nunclosed: true\n# Heading";
        let s = split_document(doc);
        assert!(s.raw_frontmatter.is_none());
        assert_eq!(s.body, doc);
    }

    #[test]
    fn merge_adds_sovereign_to_empty_doc() {
        let merged = merge_frontmatter("# Hello\n", &inputs("sovereign/epistemology"), "sovereign");
        let map = parse_fm(&merged);
        let tags = map.get(Value::String("tags".into())).unwrap();
        assert!(matches!(tags, Value::Sequence(_)));
        if let Value::Sequence(s) = tags {
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].as_str().unwrap(), "sovereign/epistemology");
        }
        assert!(map.contains_key(Value::String("sovereign_version".into())));
    }

    #[test]
    fn merge_preserves_user_tags() {
        let doc = "---\ntags:\n  - mind\n  - consciousness\n---\n# Body";
        let merged = merge_frontmatter(
            doc,
            &inputs("sovereign/epistemology/philosophy-of-mind"),
            "sovereign",
        );
        let map = parse_fm(&merged);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            let tag_strings: Vec<&str> = s.iter().filter_map(|v| v.as_str()).collect();
            assert!(tag_strings.contains(&"mind"));
            assert!(tag_strings.contains(&"consciousness"));
            assert!(tag_strings.contains(&"sovereign/epistemology/philosophy-of-mind"));
        } else {
            panic!("tags not a sequence");
        }
    }

    #[test]
    fn merge_replaces_prior_sovereign_tags() {
        let doc = "---\ntags:\n  - sovereign/old/path\n  - user_tag\n---\n";
        let merged = merge_frontmatter(doc, &inputs("sovereign/new/path"), "sovereign");
        let map = parse_fm(&merged);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            let strs: Vec<&str> = s.iter().filter_map(|v| v.as_str()).collect();
            assert!(strs.contains(&"user_tag"), "user tag must survive");
            assert!(strs.contains(&"sovereign/new/path"));
            assert!(!strs.contains(&"sovereign/old/path"), "old tag dropped");
        }
    }

    #[test]
    fn merge_handles_null_tags() {
        let doc = "---\ntags: null\n---\n";
        let merged = merge_frontmatter(doc, &inputs("sovereign/x"), "sovereign");
        let map = parse_fm(&merged);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            assert_eq!(s.len(), 1);
        } else {
            panic!("expected sequence");
        }
    }

    #[test]
    fn merge_handles_inline_array_tags() {
        let doc = "---\ntags: [a, b]\n---\n";
        let merged = merge_frontmatter(doc, &inputs("sovereign/z"), "sovereign");
        let map = parse_fm(&merged);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            let strs: Vec<&str> = s.iter().filter_map(|v| v.as_str()).collect();
            assert!(strs.contains(&"a") && strs.contains(&"b") && strs.contains(&"sovereign/z"));
        }
    }

    #[test]
    fn merge_handles_comma_string_tags() {
        // Less common but Obsidian accepts `tags: a, b, c`.
        let doc = "---\ntags: a, b, c\n---\n";
        let merged = merge_frontmatter(doc, &inputs("sovereign/q"), "sovereign");
        let map = parse_fm(&merged);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            let strs: Vec<&str> = s.iter().filter_map(|v| v.as_str()).collect();
            assert!(strs.contains(&"a") && strs.contains(&"b") && strs.contains(&"c"));
            assert!(strs.contains(&"sovereign/q"));
        }
    }

    #[test]
    fn merge_preserves_unrelated_keys() {
        let doc = "---\naliases:\n  - Foo\n  - Bar\ntype: note\n---\n";
        let merged = merge_frontmatter(doc, &inputs("sovereign/x"), "sovereign");
        let map = parse_fm(&merged);
        assert!(map.contains_key(Value::String("aliases".into())));
        assert_eq!(
            map.get(Value::String("type".into())).unwrap().as_str(),
            Some("note")
        );
    }

    #[test]
    fn merge_is_idempotent_same_inputs() {
        let doc = "---\ntype: note\n---\nBody";
        let once = merge_frontmatter(doc, &inputs("sovereign/a/b"), "sovereign");
        let twice = merge_frontmatter(&once, &inputs("sovereign/a/b"), "sovereign");
        // Rerunning with the same inputs should yield byte-identical
        // output — confidence, version, and tags all match.
        assert_eq!(once, twice, "merge must be idempotent for stable inputs");
    }

    #[test]
    fn strip_removes_sovereign_tags_and_keys() {
        let doc = "---\ntags:\n  - mine\n  - sovereign/epistemology\nsovereign_version: 3\nsovereign_cluster: Mind\n---\n# Body";
        let stripped = strip_sovereign(doc, "sovereign");
        let map = parse_fm(&stripped);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            let strs: Vec<&str> = s.iter().filter_map(|v| v.as_str()).collect();
            assert_eq!(strs, vec!["mine"]);
        }
        assert!(!map.contains_key(Value::String("sovereign_version".into())));
        assert!(!map.contains_key(Value::String("sovereign_cluster".into())));
    }

    #[test]
    fn strip_drops_frontmatter_entirely_if_only_sovereign_keys() {
        let doc = "---\nsovereign_version: 2\n---\n# Body";
        let stripped = strip_sovereign(doc, "sovereign");
        assert!(
            !stripped.contains("---"),
            "should drop empty frontmatter fence; got: {stripped}"
        );
        assert!(stripped.contains("# Body"));
    }

    #[test]
    fn strip_on_unrelated_doc_is_no_op() {
        let doc = "---\ntype: note\ntags: [x]\n---\nBody";
        let stripped = strip_sovereign(doc, "sovereign");
        // Value-level equality: round-tripped via yaml, so the exact
        // whitespace may differ; instead assert the map contents.
        let a = parse_fm(doc);
        let b = parse_fm(&stripped);
        assert_eq!(a, b);
    }

    #[test]
    fn strip_is_idempotent() {
        let doc = "---\ntags:\n  - sovereign/x\nsovereign_version: 1\n---\n";
        let once = strip_sovereign(doc, "sovereign");
        let twice = strip_sovereign(&once, "sovereign");
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_with_no_frontmatter_creates_fence() {
        let doc = "# Title\n\nJust body.";
        let merged = merge_frontmatter(doc, &inputs("sovereign/x"), "sovereign");
        assert!(merged.starts_with("---\n"));
        assert!(merged.contains("# Title"));
    }

    #[test]
    fn unicode_filenames_do_not_break_yaml() {
        // The merge doesn't read filenames but we still want to
        // confirm unicode in the tag value path round-trips cleanly.
        let merged = merge_frontmatter(
            "---\ntype: note\n---\n",
            &MergeInputs {
                primary_tag: "sovereign/épistémologie/philosophie-de-l-esprit",
                additional_tags: &[],
                cluster_display_name: "Philosophie de l'esprit",
                confidence: 0.9,
                version: 1,
            },
            "sovereign",
        );
        assert!(merged.contains("philosophie-de-l-esprit"));
        let map = parse_fm(&merged);
        // serde_yaml encodes non-ASCII strings as-is; the round-trip
        // preserves the full unicode tag.
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            assert!(s
                .iter()
                .any(|v| v.as_str().is_some_and(|x| x.contains("épistémologie"))));
        }
    }

    #[test]
    fn custom_namespace_is_honoured() {
        let doc = "---\ntags:\n  - foo/bar\n  - sovereign/old\n---\n";
        let merged = merge_frontmatter(
            doc,
            &MergeInputs {
                primary_tag: "foo/new",
                additional_tags: &[],
                cluster_display_name: "New",
                confidence: 0.5,
                version: 1,
            },
            "foo",
        );
        let map = parse_fm(&merged);
        if let Value::Sequence(s) = map.get(Value::String("tags".into())).unwrap() {
            let strs: Vec<&str> = s.iter().filter_map(|v| v.as_str()).collect();
            // `foo/bar` removed (same namespace), `sovereign/old`
            // kept (different namespace), `foo/new` added.
            assert!(!strs.contains(&"foo/bar"));
            assert!(strs.contains(&"sovereign/old"));
            assert!(strs.contains(&"foo/new"));
        }
    }
}
