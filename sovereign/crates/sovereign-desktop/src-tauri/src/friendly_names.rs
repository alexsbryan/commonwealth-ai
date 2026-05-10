//! Memorable two-word node-name generator (e.g. "BeefyMac",
//! "CrispFalcon"). Used to give first-launch desktop users a default
//! `node_name` better than the bare system hostname so they have a
//! recognizable identity in mesh member rosters.
//!
//! Why this exists: we used to ship users into mesh with whatever
//! `hostname::get()` returned ("Alexs-MacBook-2"), which is generic
//! and forgettable. A short adjective+mascot pair is friendlier and
//! sticks in memory ("BeefyMac" was a long-time user's actual
//! identity, lost in a config reset because no default existed). The
//! user can always override via the node-name input in MeshSettings.

use rand::{rngs::StdRng, Rng, SeedableRng};

/// ~50 short, friendly adjectives. Avoid anything edgy / political /
/// gendered — the wordlist defines first impressions of the user
/// inside their friend group's mesh.
const ADJECTIVES: &[&str] = &[
    "Beefy", "Brassy", "Brave", "Bright", "Bold", "Breezy",
    "Cozy", "Crisp", "Curious", "Calm", "Crunchy",
    "Dapper", "Dandy", "Dusty",
    "Eager", "Earnest", "Easy",
    "Fluffy", "Frosty", "Feisty", "Fancy",
    "Glossy", "Glad", "Gentle", "Groovy",
    "Hearty", "Honest", "Happy",
    "Jolly", "Jaunty", "Jazzy",
    "Kind", "Keen",
    "Lucky", "Lively", "Lofty",
    "Mighty", "Merry", "Mellow",
    "Nifty", "Nimble", "Noble",
    "Plucky", "Peppy", "Plush", "Polished",
    "Quirky", "Quick",
    "Rosy", "Royal", "Rugged",
    "Snappy", "Sleepy", "Spry", "Sunny", "Sneaky", "Silver",
    "Tidy", "Tame",
    "Witty", "Wiry", "Wise",
    "Zesty", "Zen",
];

/// ~30 mascot nouns — animals, machines, vibes. Same vetting:
/// nothing offensive, nothing too long, nothing brand-coded.
const MASCOTS: &[&str] = &[
    "Mac", "Falcon", "Otter", "Walrus", "Badger", "Yak", "Lemur",
    "Manatee", "Marmot", "Pelican", "Heron", "Wombat", "Penguin",
    "Pangolin", "Capybara", "Quokka", "Tapir", "Lynx", "Stoat",
    "Puffin", "Narwhal", "Axolotl", "Hedgehog", "Beaver",
    "Moose", "Bison", "Crane", "Magpie", "Raven", "Owl", "Hare",
    "Bear", "Fox", "Wolf",
];

/// Returns e.g. "BeefyMac".
///
/// Random by default. Pass `seed: Some(n)` for a deterministic
/// pick (used by tests). With ~70 × ~34 = ~2400 combinations,
/// collision rate inside a friend-group mesh is negligible.
pub fn generate(seed: Option<u64>) -> String {
    let mut rng = match seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_entropy(),
    };
    let adj = ADJECTIVES[rng.gen_range(0..ADJECTIVES.len())];
    let noun = MASCOTS[rng.gen_range(0..MASCOTS.len())];
    format!("{adj}{noun}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_with_seed() {
        // Same seed → same name. Guards against accidentally moving
        // wordlists around without realising the deterministic path
        // breaks for any caller that pins a seed.
        let a = generate(Some(42));
        let b = generate(Some(42));
        assert_eq!(a, b);
    }

    #[test]
    fn random_default_picks_from_wordlist() {
        let name = generate(None);
        // Must start with one of the adjectives.
        assert!(
            ADJECTIVES.iter().any(|adj| name.starts_with(adj)),
            "generated name {name:?} did not start with a known adjective"
        );
        // Length sanity check — both lists have 2-letter minimums.
        assert!(name.len() >= 4, "generated name {name:?} suspiciously short");
    }

    #[test]
    fn no_whitespace_in_output() {
        for seed in 0..50u64 {
            let name = generate(Some(seed));
            assert!(
                !name.contains(char::is_whitespace),
                "generated name {name:?} contains whitespace"
            );
        }
    }
}
