//! PII scrubber primitive — replaces structured PII (emails, phones,
//! URLs, money, IPs, ISO dates) via regex, and replaces names/orgs
//! that the caller has explicitly registered with stable role tokens
//! like `[[person-1]]` / `[[org-2]]`.
//!
//! **Scope.** Producer-side sanitization for artifacts that leave the
//! local box — bench question banks, baselines, debug fixtures.
//! Corpus chunks themselves are NEVER scrubbed; they stay raw under
//! `~/.sovereign/corpora/<id>/`. Use this when deriving anything
//! committed to the repo from a sensitive corpus.
//!
//! **Not an NER engine.** Name discovery is out of scope. Seed the
//! map from a source the caller trusts — for conversation corpora
//! that's the existing Phase 1 atom extractor's `Person` atoms; for
//! ad-hoc use it's `register_person`/`register_org` calls. Keeping
//! discovery out of this module means it's deterministic, has no
//! model dependency, and is unit-testable as pure text → text.
//!
//! **Token format.** `[[kind-N]]` is wiki-link style: visually
//! distinct, survives both markdown rendering and JSON encoding
//! without escaping, and is trivial to recognise with a single
//! regex so re-scrubbing already-scrubbed text is a no-op.
//!
//! **Stability.** Token indices are assigned in insertion order and
//! persist via `EntityMap::save`/`load`. Re-running scrub against
//! the same loaded map produces identical output; a fresh map
//! produces stable output within a single process run.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Org,
    Email,
    Phone,
    Url,
    Money,
    Ipv4,
    IsoDate,
}

impl EntityKind {
    pub fn slug(self) -> &'static str {
        match self {
            EntityKind::Person => "person",
            EntityKind::Org => "org",
            EntityKind::Email => "email",
            EntityKind::Phone => "phone",
            EntityKind::Url => "url",
            EntityKind::Money => "money",
            EntityKind::Ipv4 => "ipv4",
            EntityKind::IsoDate => "iso-date",
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EntityMap {
    persons: Bucket,
    orgs: Bucket,
    structured: HashMap<String, String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Bucket {
    surfaces: Vec<String>,
    lookup: HashMap<String, usize>,
}

impl Bucket {
    fn register(&mut self, surface: &str) -> usize {
        let key = surface.trim().to_lowercase();
        if let Some(&idx) = self.lookup.get(&key) {
            return idx;
        }
        let idx = self.surfaces.len() + 1;
        self.surfaces.push(surface.trim().to_string());
        self.lookup.insert(key, idx);
        idx
    }

    fn token_for(&self, surface: &str, kind: EntityKind) -> Option<String> {
        let key = surface.trim().to_lowercase();
        self.lookup
            .get(&key)
            .map(|i| format!("[[{}-{}]]", kind.slug(), i))
    }

    fn contains(&self, surface: &str) -> bool {
        self.lookup.contains_key(&surface.trim().to_lowercase())
    }
}

#[derive(Debug, Default, Clone)]
pub struct ScrubResult {
    pub text: String,
    pub replacements: HashMap<EntityKind, usize>,
}

impl EntityMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_person(&mut self, surface: &str) -> String {
        let idx = self.persons.register(surface);
        format!("[[person-{}]]", idx)
    }

    pub fn register_org(&mut self, surface: &str) -> String {
        let idx = self.orgs.register(surface);
        format!("[[org-{}]]", idx)
    }

    pub fn seed_persons<I, S>(&mut self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for n in names {
            self.persons.register(n.as_ref());
        }
    }

    pub fn seed_orgs<I, S>(&mut self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for n in names {
            self.orgs.register(n.as_ref());
        }
    }

    pub fn token_for_person(&self, surface: &str) -> Option<String> {
        self.persons.token_for(surface, EntityKind::Person)
    }

    pub fn token_for_org(&self, surface: &str) -> Option<String> {
        self.orgs.token_for(surface, EntityKind::Org)
    }

    pub fn person_count(&self) -> usize {
        self.persons.surfaces.len()
    }

    pub fn org_count(&self) -> usize {
        self.orgs.surfaces.len()
    }

    /// Produce a name guaranteed not to collide with any registered
    /// person. Useful for negative-sample bench questions ("have I
    /// ever discussed X" where X is constructed-not-in-corpus).
    /// Deterministic given `seed`.
    pub fn unmapped_person(&self, seed: u64) -> String {
        let pool = [
            "Avery Nakamura",
            "Quinn Salazar",
            "Rhea Okafor",
            "Soren Vasquez",
            "Theodora Lin",
            "Wynn Petrosian",
            "Calliope Ferrer",
            "Idris Bergstrom",
            "Marisol Trent",
            "Beatrix Holloway",
        ];
        let mut cursor = seed as usize;
        loop {
            let pick = pool[cursor % pool.len()];
            if !self.persons.contains(pick) {
                return pick.to_string();
            }
            cursor = cursor.wrapping_add(1);
            // The pool is small but the bench needs unique negatives,
            // not many — bail rather than loop forever on adversarial
            // seeded maps.
            if cursor > seed as usize + pool.len() {
                return format!("Unmapped Person {}", seed);
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, data)
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

fn structured_regexes() -> &'static [(EntityKind, Regex)] {
    static CELL: OnceLock<Vec<(EntityKind, Regex)>> = OnceLock::new();
    CELL.get_or_init(|| {
        vec![
            (
                EntityKind::Email,
                Regex::new(r"(?i)\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b").unwrap(),
            ),
            (
                EntityKind::Url,
                Regex::new(r"(?i)\bhttps?://[^\s\)\]\}>]+").unwrap(),
            ),
            (
                EntityKind::Ipv4,
                Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap(),
            ),
            (
                EntityKind::Phone,
                Regex::new(
                    r"(?x)
                    (?:\+?\d{1,3}[\s\-\.]?)?
                    (?:\(?\d{3}\)?[\s\-\.]?)
                    \d{3}[\s\-\.]?\d{4}
                    \b",
                )
                .unwrap(),
            ),
            (
                EntityKind::Money,
                Regex::new(
                    r"(?x)
                    \$\s?\d{1,3}(?:,\d{3})*(?:\.\d+)?
                    (?:\s?[KkMmBb])?
                    ",
                )
                .unwrap(),
            ),
            (
                EntityKind::IsoDate,
                Regex::new(r"\b\d{4}-\d{2}-\d{2}\b").unwrap(),
            ),
        ]
    })
}

fn token_regex() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"\[\[[a-z\-]+-\d+\]\]").unwrap())
}

/// Build a case-insensitive, word-boundary-anchored regex matching
/// any of `surfaces`. Longer surfaces first so "Alex Bryan" wins
/// over "Alex". Returns None on empty input.
fn build_alternation_regex(surfaces: &[String]) -> Option<Regex> {
    if surfaces.is_empty() {
        return None;
    }
    let mut sorted: Vec<&str> = surfaces.iter().map(String::as_str).collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let alts: Vec<String> = sorted.iter().map(|s| regex::escape(s)).collect();
    let pattern = format!(r"(?i)\b(?:{})\b", alts.join("|"));
    Regex::new(&pattern).ok()
}

/// Scrub `text` against `map`. Structured PII is detected by regex;
/// names/orgs are replaced only if they were registered on `map`.
/// Already-tokenized spans (`[[kind-N]]`) are skipped so the function
/// is idempotent under repeated application.
pub fn scrub_pii(text: &str, map: &mut EntityMap) -> ScrubResult {
    let mut counts: HashMap<EntityKind, usize> = HashMap::new();

    // Phase 1 — protect existing tokens by extracting them into
    // placeholders, scrubbing the remainder, then splicing back.
    let tokens: Vec<(usize, usize, String)> = token_regex()
        .find_iter(text)
        .map(|m| (m.start(), m.end(), m.as_str().to_string()))
        .collect();

    let mut working = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut spans: Vec<String> = Vec::new();
    for (start, end, tok) in &tokens {
        working.push_str(&text[cursor..*start]);
        let placeholder = format!("\u{0001}TOK{}\u{0001}", spans.len());
        working.push_str(&placeholder);
        spans.push(tok.clone());
        cursor = *end;
    }
    working.push_str(&text[cursor..]);

    // Phase 2 — structured PII.
    for (kind, re) in structured_regexes() {
        let prefix = format!("{}::", kind.slug());
        let replaced = re.replace_all(&working, |caps: &regex::Captures<'_>| {
            let raw = caps.get(0).unwrap().as_str().to_string();
            let key = format!("{}{}", prefix, raw.to_lowercase());
            let tok = if let Some(existing) = map.structured.get(&key) {
                existing.clone()
            } else {
                let bucket_size = map
                    .structured
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .count()
                    + 1;
                let new_tok = format!("[[{}-{}]]", kind.slug(), bucket_size);
                map.structured.insert(key, new_tok.clone());
                new_tok
            };
            *counts.entry(*kind).or_insert(0) += 1;
            tok
        });
        working = replaced.into_owned();
    }

    // Phase 3 — persons + orgs from registered surfaces.
    if let Some(re) = build_alternation_regex(&map.persons.surfaces) {
        let persons = map.persons.clone();
        working = re
            .replace_all(&working, |caps: &regex::Captures<'_>| {
                let raw = caps.get(0).unwrap().as_str();
                let tok = persons
                    .token_for(raw, EntityKind::Person)
                    .unwrap_or_else(|| raw.to_string());
                *counts.entry(EntityKind::Person).or_insert(0) += 1;
                tok
            })
            .into_owned();
    }
    if let Some(re) = build_alternation_regex(&map.orgs.surfaces) {
        let orgs = map.orgs.clone();
        working = re
            .replace_all(&working, |caps: &regex::Captures<'_>| {
                let raw = caps.get(0).unwrap().as_str();
                let tok = orgs
                    .token_for(raw, EntityKind::Org)
                    .unwrap_or_else(|| raw.to_string());
                *counts.entry(EntityKind::Org).or_insert(0) += 1;
                tok
            })
            .into_owned();
    }

    // Phase 4 — restore protected tokens.
    let mut out = working;
    for (i, original) in spans.iter().enumerate() {
        out = out.replace(&format!("\u{0001}TOK{}\u{0001}", i), original);
    }

    ScrubResult {
        text: out,
        replacements: counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_scrubbed_to_token() {
        let mut map = EntityMap::new();
        let out = scrub_pii("Contact me at alex@example.com please.", &mut map);
        assert!(out.text.contains("[[email-1]]"));
        assert!(!out.text.contains("alex@example.com"));
        assert_eq!(out.replacements.get(&EntityKind::Email).copied(), Some(1));
    }

    #[test]
    fn same_email_gets_same_token_within_run() {
        let mut map = EntityMap::new();
        let out = scrub_pii("a@b.com then a@b.com again", &mut map);
        let count = out.text.matches("[[email-1]]").count();
        assert_eq!(count, 2);
        assert!(!out.text.contains("[[email-2]]"));
    }

    #[test]
    fn distinct_emails_get_distinct_tokens() {
        let mut map = EntityMap::new();
        let out = scrub_pii("a@b.com vs c@d.com", &mut map);
        assert!(out.text.contains("[[email-1]]"));
        assert!(out.text.contains("[[email-2]]"));
    }

    #[test]
    fn phone_url_money_ipv4_iso_date_all_caught() {
        let mut map = EntityMap::new();
        let raw = "Call 415-555-1212 see https://acme.example.com paid $2,400.50 from 10.0.0.5 on 2025-08-14.";
        let out = scrub_pii(raw, &mut map);
        assert!(out.text.contains("[[phone-1]]"), "phone: {}", out.text);
        assert!(out.text.contains("[[url-1]]"));
        assert!(out.text.contains("[[money-1]]"));
        assert!(out.text.contains("[[ipv4-1]]"));
        assert!(out.text.contains("[[iso-date-1]]"));
    }

    #[test]
    fn registered_person_replaced_case_insensitive_word_boundary() {
        let mut map = EntityMap::new();
        map.register_person("Alex Bryan");
        let out = scrub_pii(
            "Alex Bryan met with alex bryan. Alexander stayed home.",
            &mut map,
        );
        assert!(out.text.contains("[[person-1]] met with [[person-1]]"));
        assert!(
            out.text.contains("Alexander"),
            "must not match inside Alexander: {}",
            out.text
        );
    }

    #[test]
    fn longest_match_wins_on_overlapping_names() {
        let mut map = EntityMap::new();
        map.register_person("Alex");
        map.register_person("Alex Bryan");
        let out = scrub_pii("Alex Bryan ate. Alex slept.", &mut map);
        // "Alex Bryan" matched whole, not as "Alex" + " Bryan".
        assert!(out.text.contains("[[person-2]] ate"));
        assert!(out.text.contains("[[person-1]] slept"));
    }

    #[test]
    fn org_replacement_works() {
        let mut map = EntityMap::new();
        map.register_org("Acme Corp");
        let out = scrub_pii("Acme Corp signed it.", &mut map);
        assert!(out.text.contains("[[org-1]]"));
    }

    #[test]
    fn unregistered_name_left_alone() {
        let mut map = EntityMap::new();
        let out = scrub_pii("Some random Person Name appeared.", &mut map);
        assert_eq!(out.text, "Some random Person Name appeared.");
    }

    #[test]
    fn idempotent_under_repeat_scrub() {
        let mut map = EntityMap::new();
        map.register_person("Alex Bryan");
        let once = scrub_pii("Alex Bryan at alex@example.com on 2025-08-14", &mut map);
        let twice = scrub_pii(&once.text, &mut map);
        assert_eq!(
            once.text, twice.text,
            "second scrub must not mangle existing tokens"
        );
    }

    #[test]
    fn map_round_trip_serialize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.json");
        let mut map = EntityMap::new();
        map.register_person("Alex Bryan");
        map.register_org("Acme Corp");
        let _ = scrub_pii("see alex@example.com", &mut map);
        map.save(&path).unwrap();

        let loaded = EntityMap::load(&path).unwrap();
        assert_eq!(
            loaded.token_for_person("alex bryan").as_deref(),
            Some("[[person-1]]")
        );
        assert_eq!(
            loaded.token_for_org("ACME CORP").as_deref(),
            Some("[[org-1]]")
        );
        assert_eq!(loaded.person_count(), 1);
    }

    #[test]
    fn unmapped_person_avoids_collisions() {
        let mut map = EntityMap::new();
        map.register_person("Avery Nakamura");
        let name = map.unmapped_person(0);
        assert_ne!(name, "Avery Nakamura");
        assert!(map.token_for_person(&name).is_none());
    }

    #[test]
    fn seed_persons_bulk_loads() {
        let mut map = EntityMap::new();
        map.seed_persons(["Alex", "Jordan", "Sam"]);
        assert_eq!(map.person_count(), 3);
        assert_eq!(
            map.token_for_person("jordan").as_deref(),
            Some("[[person-2]]")
        );
    }

    #[test]
    fn tokens_inside_input_are_protected() {
        let mut map = EntityMap::new();
        map.register_person("Alex");
        // Input already has a token; scrub must not double-tokenize
        // or mistake "[[person-99]]" for new text.
        let out = scrub_pii("Alex saw [[person-99]] at noon.", &mut map);
        assert!(out.text.contains("[[person-1]] saw [[person-99]] at noon."));
    }
}
