//! Bullet-grain chunker for `Portal:Current_events` Wikipedia pages.
//!
//! A Portal page's wikitext (after `wikipedia_api_article` extraction
//! produces one `ExtractedDoc` per H3 section) reads roughly:
//!
//! ```wiki
//! ; Armed conflicts and attacks
//! * Russian invasion of Ukraine: At least 12 killed in [[Kyiv]]…
//! ** Ukrainian forces retake village near [[Kupiansk]]…
//! * [[Yemeni civil war]]: …
//! ; Politics and elections
//! * [[2026 Australian federal election]]: …
//! ```
//!
//! Retrieval lives at the bullet grain — "what was logged on day X" reads
//! more naturally when each event is its own chunk. Sub-bullets fold
//! under their parent so the surrounding context survives.
//!
//! The chunker also exposes a tiny utility — `extract_bullet_links` —
//! that returns the set of `[[…]]` wikilink targets within a single
//! bullet's text. The watcher uses it to attribute a portal page's
//! outgoing-link list to the specific bullet that generated each link,
//! so the per-chunk `outbound_links` JSON field carries only the links
//! that *appear in that bullet*.

use super::{Chunker, TextChunk};

pub struct PortalEventBulletChunker {
    pub max_chars: usize,
}

impl PortalEventBulletChunker {
    pub const DEFAULT_MAX_CHARS: usize = 2048;

    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Default for PortalEventBulletChunker {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_CHARS)
    }
}

impl Chunker for PortalEventBulletChunker {
    fn chunk(&self, text: &str) -> Vec<TextChunk> {
        let bullets = split_bullets(text, self.max_chars);
        bullets
            .into_iter()
            .enumerate()
            .map(|(index, content)| TextChunk { content, index })
            .collect()
    }
}

/// Split a Portal:Current_events section body into per-event bullet
/// chunks. A "bullet" is a `*`-prefixed line plus any deeper-nested
/// `**`/`***` sub-bullets that follow it before the next top-level `*`.
///
/// Lines that aren't part of any bullet (e.g. an empty line, the `;` H3
/// heading the extractor leaves in place, or a `</noinclude>` wrapper)
/// are dropped — they carry no event content. If the splitter finds no
/// bullets at all, it falls back to emitting the whole text as one
/// chunk so the page's metadata isn't lost.
fn split_bullets(text: &str, max_chars: usize) -> Vec<String> {
    let mut bullets: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = top_level_bullet(trimmed) {
            // Close out the previous bullet, if any, and start a new one.
            if let Some(prev) = current.take() {
                push_bounded(&mut bullets, prev, max_chars);
            }
            current = Some(rest.trim().to_string());
        } else if is_sub_bullet(trimmed) {
            // Fold sub-bullets onto whatever bullet is open. If we
            // have no current bullet (a sub-bullet opening a section
            // is malformed but possible), treat it as a new top-level.
            let body = trimmed.trim_start_matches('*').trim().to_string();
            match current.as_mut() {
                Some(buf) => {
                    buf.push('\n');
                    buf.push_str("- ");
                    buf.push_str(&body);
                }
                None => current = Some(body),
            }
        } else {
            // Not a bullet line — flush the current bullet if open.
            if let Some(prev) = current.take() {
                push_bounded(&mut bullets, prev, max_chars);
            }
        }
    }
    if let Some(prev) = current.take() {
        push_bounded(&mut bullets, prev, max_chars);
    }

    if bullets.is_empty() && !text.trim().is_empty() {
        // No bullet markup at all (maybe the extractor produced a
        // sectionless page). Don't lose the content — return one chunk.
        bullets.push(text.trim().to_string());
    }
    bullets
}

/// Returns the bullet body (everything after `*` plus any whitespace) if
/// `line` is a top-level bullet (a single leading `*`, not `**`/`***`).
fn top_level_bullet(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    if chars.next()? != '*' {
        return None;
    }
    if chars.next() == Some('*') {
        return None; // ** sub-bullet
    }
    Some(line.strip_prefix('*').unwrap())
}

fn is_sub_bullet(line: &str) -> bool {
    line.starts_with("**")
}

/// Append `bullet` to `out`. If the bullet exceeds `max_chars`, split on
/// sentence-ish boundaries to avoid producing a single oversized chunk.
fn push_bounded(out: &mut Vec<String>, bullet: String, max_chars: usize) {
    if max_chars == 0 || bullet.chars().count() <= max_chars {
        if !bullet.trim().is_empty() {
            out.push(bullet);
        }
        return;
    }
    // Bullets longer than max_chars (rare — usually means a sub-bullet
    // tower) split at sentence boundaries. Use a simple `. ` split, then
    // re-pack greedily so we don't emit a flood of tiny chunks.
    let mut current = String::new();
    for piece in bullet.split_inclusive(['.', '!', '?']) {
        if current.chars().count() + piece.chars().count() > max_chars && !current.is_empty() {
            out.push(std::mem::take(&mut current).trim().to_string());
        }
        current.push_str(piece);
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
}

/// Extract `[[Article]]` and `[[Article|alias]]` wikilink targets from a
/// bullet's text. Targets are returned with underscores in place of
/// spaces and case as written — the watcher normalises further before
/// hashing into MeshStore keys.
///
/// File: and Image: links are dropped (they're media references, not
/// article targets); section anchors `[[Foo#Bar]]` collapse to `Foo`.
pub fn extract_bullet_links(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            let tail = &text[i + 2..];
            let close = tail.find("]]");
            let next_open = tail.find("[[");
            // If another `[[` appears before the closing `]]`, the
            // current opener is malformed (nested or unclosed). Skip
            // one byte and let the inner opener be retried on the
            // next iteration.
            match (close, next_open) {
                (Some(c), Some(n)) if n < c => {
                    i += 1;
                    continue;
                }
                (Some(c), _) => {
                    let inner = &tail[..c];
                    if let Some(target) = parse_wikilink_target(inner) {
                        if !out.contains(&target) {
                            out.push(target);
                        }
                    }
                    i += 2 + c + 2;
                    continue;
                }
                (None, _) => {
                    // No more closers in the rest of the document; bail.
                    break;
                }
            }
        }
        i += 1;
    }
    out
}

fn parse_wikilink_target(inner: &str) -> Option<String> {
    let head = inner.split('|').next().unwrap_or(inner).trim();
    if head.is_empty() {
        return None;
    }
    // File:/Image:/Category: are not article references — drop.
    let lc = head.to_ascii_lowercase();
    if lc.starts_with("file:")
        || lc.starts_with("image:")
        || lc.starts_with("category:")
        || lc.starts_with(":file:")
        || lc.starts_with(":category:")
    {
        return None;
    }
    // Drop fragment.
    let title = head.split('#').next().unwrap_or(head).trim();
    if title.is_empty() {
        return None;
    }
    Some(title.replace(' ', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_top_level_bullets_into_one_chunk_each() {
        let text = "\
* First event in [[Kyiv]] today.
* Second event mentions [[OPEC]] and [[Saudi_Arabia]].
* Third event with [[2026 Australian federal election|the election]].
";
        let chunks = PortalEventBulletChunker::default().chunk(text);
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].content.contains("First event"));
        assert!(chunks[1].content.contains("OPEC"));
        assert!(chunks[2].content.contains("Australian federal"));
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.index, i);
        }
    }

    #[test]
    fn folds_sub_bullets_under_parent() {
        let text = "\
* Russian invasion of Ukraine: At least 12 killed in [[Kyiv]].
** Ukrainian forces retake village near [[Kupiansk]].
** Russian Defense Ministry confirms missile loss.
* [[Yemeni civil war]]: Houthi statement.
";
        let chunks = PortalEventBulletChunker::default().chunk(text);
        assert_eq!(chunks.len(), 2, "two top-level bullets, sub-bullets fold");
        assert!(chunks[0].content.contains("Russian invasion"));
        assert!(chunks[0].content.contains("Kupiansk"));
        assert!(chunks[0].content.contains("missile loss"));
        assert!(chunks[1].content.contains("Yemeni civil war"));
    }

    #[test]
    fn drops_non_bullet_lines() {
        let text = "\
; Armed conflicts and attacks
* First event.

* Second event.
<noinclude>
* Third event.
";
        let chunks = PortalEventBulletChunker::default().chunk(text);
        assert_eq!(chunks.len(), 3);
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        assert!(PortalEventBulletChunker::default().chunk("").is_empty());
        assert!(PortalEventBulletChunker::default().chunk("\n\n  \n").is_empty());
    }

    #[test]
    fn no_bullets_falls_back_to_single_chunk() {
        // Content with no bullet markup must still survive — losing the
        // page would be the worst-of-all outcomes.
        let text = "Just a paragraph of prose with no markdown bullets at all.";
        let chunks = PortalEventBulletChunker::default().chunk(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, text);
    }

    #[test]
    fn oversized_bullet_splits_at_sentence_boundary() {
        // A 600-char bullet with a tight max should split into multiple
        // chunks, each under the limit.
        let sentence = "This is a sentence with [[Some_Article]] mention. ".repeat(15);
        let text = format!("* {sentence}");
        let chunker = PortalEventBulletChunker::new(200);
        let chunks = chunker.chunk(&text);
        assert!(chunks.len() > 1, "expected oversize bullet to split");
        for c in &chunks {
            assert!(
                c.content.chars().count() <= 200 + 60, // small slack for sentence-inclusive splits
                "chunk too long: {} chars",
                c.content.chars().count(),
            );
        }
    }

    #[test]
    fn extracts_simple_wikilink() {
        let links = extract_bullet_links("Body mentions [[Kyiv]] briefly.");
        assert_eq!(links, vec!["Kyiv".to_string()]);
    }

    #[test]
    fn extracts_aliased_wikilink_target() {
        let links = extract_bullet_links("[[Donald Trump|the president]] said ...");
        assert_eq!(links, vec!["Donald_Trump".to_string()]);
    }

    #[test]
    fn dedupes_repeated_targets() {
        let links = extract_bullet_links("[[Russia]] and [[Russia]] again [[Russia|Russian state]]");
        assert_eq!(links, vec!["Russia".to_string()]);
    }

    #[test]
    fn drops_section_anchors() {
        let links = extract_bullet_links("[[Bismarck#Politics|the chancellor]] resigned.");
        assert_eq!(links, vec!["Bismarck".to_string()]);
    }

    #[test]
    fn drops_file_image_category_links() {
        let links = extract_bullet_links(
            "[[File:logo.png]] [[Image:flag.svg|alt]] [[Category:Politics]] but keep [[Iceland]].",
        );
        assert_eq!(links, vec!["Iceland".to_string()]);
    }

    #[test]
    fn handles_unclosed_bracket_gracefully() {
        let links = extract_bullet_links("[[Unclosed without closer ... [[Iceland]] follows.");
        assert!(links.contains(&"Iceland".to_string()));
    }

    #[test]
    fn empty_bullet_is_dropped() {
        let text = "* \n* Real event.\n*\n";
        let chunks = PortalEventBulletChunker::default().chunk(text);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("Real event"));
    }
}
