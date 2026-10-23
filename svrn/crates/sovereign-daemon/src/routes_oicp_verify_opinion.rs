// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opinion at verify's door (OICP v0.6 §2.6, rung `class/opinion`): a claim
//! holding one of the markers in the data file
//! (`svrn/crates/sovereign-daemon/data/opinion_markers.toml`) is offered as
//! the writer's view and is `not_checkable`, decided before any search or
//! model call. The rule is data; this is only its matcher, a port of
//! zoracite's `explorations/v2-book-figures/door.py` (`_pattern`, c825ca3).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use kernel_types::Sha256Hash;

/// The data file, vendored into the binary.
pub const DATA_FILE: &str = include_str!("../data/opinion_markers.toml");

/// One marker: its group, as written, and its words lowercased.
struct Marker {
    group: String,
    text: String,
    words: Vec<Vec<char>>,
}

/// The markers, and an identity that changes whenever what they decide can.
pub struct OpinionRule {
    markers: Vec<Marker>,
    id: String,
}

impl OpinionRule {
    /// Each key of the file is a group of markers; a value that is not a list
    /// of strings, or a marker with no words, is an error, never skipped.
    pub fn parse(toml_text: &str) -> Result<Self, String> {
        let groups: BTreeMap<String, Vec<String>> =
            toml::from_str(toml_text).map_err(|e| e.to_string())?;
        let mut markers = Vec::new();
        let mut canonical = String::new();
        for (group, list) in groups {
            for text in list {
                let words: Vec<Vec<char>> = text
                    .split_whitespace()
                    .map(|w| w.to_lowercase().chars().collect())
                    .collect();
                if words.is_empty() {
                    return Err(format!("group `{group}` holds a marker with no words"));
                }
                canonical.push_str(&format!("{group}\t{text}\n"));
                markers.push(Marker {
                    group: group.clone(),
                    text,
                    words,
                });
            }
        }
        if markers.is_empty() {
            return Err("the rule holds no marker".into());
        }
        let id = Sha256Hash::of_str(&canonical).to_hex()[..12].to_string();
        Ok(OpinionRule { markers, id })
    }

    /// The (group, marker) pairs the claim holds; empty, it is offered as fact.
    pub fn markers_in(&self, claim: &str) -> Vec<(&str, &str)> {
        let claim: Vec<char> = claim.chars().collect();
        self.markers
            .iter()
            .filter(|m| holds(&claim, &m.words))
            .map(|m| (m.group.as_str(), m.text.as_str()))
            .collect()
    }

    /// Named in `verifier` as `opinion=<id>`: a digest of every group and
    /// marker, so an edit to the list is a new verifier.
    pub fn id(&self) -> &str {
        &self.id
    }
}

static RULE: LazyLock<Result<OpinionRule, String>> = LazyLock::new(|| {
    OpinionRule::parse(DATA_FILE).inspect_err(|why| {
        tracing::warn!(%why, "oicp verify: the opinion rule does not read; no claim is held at the door (verifier says opinion=none)")
    })
});

/// The shipped rule, or why it does not read. Checked by this crate's tests,
/// so a binary whose file does not parse cannot be built green.
pub fn rule() -> Result<&'static OpinionRule, &'static str> {
    RULE.as_ref().map_err(String::as_str)
}

/// What `verifier` names for the door: the rule's id, or `none`.
pub fn identity() -> String {
    format!("opinion={}", rule().map_or("none", OpinionRule::id))
}

/// A word character, or an apostrophe: what may not touch a marker's ends
/// (`door.py`: `(?<![\w'’])` and `(?![\w'’])`).
fn joins(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\'' || c == '\u{2019}'
}

fn holds(claim: &[char], words: &[Vec<char>]) -> bool {
    (0..claim.len()).any(|start| {
        (start == 0 || !joins(claim[start - 1]))
            && match_at(claim, start, words)
                .is_some_and(|end| claim.get(end).is_none_or(|&c| !joins(c)))
    })
}

/// Where the marker ends when it starts at `i`: its words in order, any
/// whitespace (at least one) between them, case-insensitive, a straight
/// apostrophe matching a curly one too.
fn match_at(claim: &[char], mut i: usize, words: &[Vec<char>]) -> Option<usize> {
    for (k, word) in words.iter().enumerate() {
        if k > 0 {
            let gap = claim[i..].iter().take_while(|c| c.is_whitespace()).count();
            if gap == 0 {
                return None;
            }
            i += gap;
        }
        for &m in word {
            let c = *claim.get(i)?;
            let same = if m == '\'' {
                c == '\'' || c == '\u{2019}'
            } else {
                c == m || c.to_lowercase().eq(std::iter::once(m))
            };
            if !same {
                return None;
            }
            i += 1;
        }
    }
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_file_reads_with_its_two_groups() {
        let r = rule().expect("the shipped opinion rule parses");
        let groups: std::collections::BTreeSet<&str> =
            r.markers.iter().map(|m| m.group.as_str()).collect();
        assert_eq!(groups, ["first_person", "judgement"].into());
        assert_eq!(r.markers.len(), 44);
        assert!(OpinionRule::parse("first_person = [\"  \"]").is_err());
        assert!(OpinionRule::parse("first_person = \"I think\"").is_err());
    }

    /// Each case's markers as `door.py`'s `rule()` returns them over
    /// door_rule.json at zoracite c825ca3 (run 2026-10-20): the port matches
    /// the matcher it was tuned with, its edges included.
    #[test]
    fn the_matcher_agrees_with_door_py() {
        let r = rule().unwrap();
        let cases: [(&str, &[&str]); 18] = [
            (
                "I think Smith overstates the 1776 tariff\u{2019}s reach.",
                &["I think", "overstates"],
            ),
            ("i THINK so.", &["I think"]),
            ("In  my\n view the tariff failed.", &["in my view"]),
            ("I\u{2019}d wager it rose.", &["I'd wager"]),
            ("I'd wager it rose.", &["I'd wager"]),
            ("I\u{2019}D WAGER it rose.", &["I'd wager"]),
            ("He was thinking of 1776.", &[]),
            ("Ithink so.", &[]),
            ("'I think so,' she said.", &[]),
            ("The overstatement ran to 40%.", &[]),
            (
                "The policy was dangerously loose in 1932.",
                &["dangerously"],
            ),
            ("It was unjustly levied in 1932.", &[]),
            ("She found 14 errors.", &[]),
            ("I_think so.", &[]),
            ("The 1776 tariff was 12 percent.", &[]),
            ("He wrongly dated it 1812.", &["wrongly"]),
            ("It strikes me as odd.", &["strikes me", "strikes me"]),
            ("I am  convinced.", &["I am convinced"]),
        ];
        for (claim, want) in cases {
            let mut got: Vec<&str> = r.markers_in(claim).into_iter().map(|(_, m)| m).collect();
            let mut want = want.to_vec();
            got.sort_unstable();
            want.sort_unstable();
            assert_eq!(got, want, "{claim:?}");
        }
    }

    #[test]
    fn the_identity_follows_the_markers_not_the_comments() {
        let a = OpinionRule::parse("g = [\"I think\"]").unwrap();
        let b = OpinionRule::parse("# a note\ng = [\"I think\"]").unwrap();
        let c = OpinionRule::parse("g = [\"I think\", \"I doubt\"]").unwrap();
        assert_eq!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        assert_eq!(identity(), format!("opinion={}", rule().unwrap().id()));
    }
}
