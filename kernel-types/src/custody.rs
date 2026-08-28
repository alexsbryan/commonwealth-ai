// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Custody`] — where a piece of content stands for sharing.
//!
//! Relocated to the kernel 2026-08-20 (noun-convergence rung nc-1-kernel)
//! from `sovereign-contracts/src/types/custody.rs`, unchanged. Custody is
//! provenance, which is what layer 0 is for, and it was living in a PRODUCT
//! domain's types crate — so `corpus-engine` naming it, at the point where
//! content is acquired and the stamp actually belongs, was a backflow edge by
//! the workspace's own layer map. `sovereign-contracts` re-exports it, so the
//! 145 existing reference sites are untouched.
//!
//! What did NOT come down: `ChunkCustody` (the grounding gate's per-chunk
//! ledger ROW) and `CUSTODY_META_KEY` (the legacy `HashMap<String,String>`
//! channel). Both are the gate's business rather than the kernel's, and the
//! string key is the channel this campaign is closing — putting it at layer 0
//! would bless it.
//!
//! Closed set → an enum, never a stringly registry (ARCH §2). The wire
//! spellings are a stable contract: the custody reds
//! (`sovereign-core/tests/custody_reds.rs`) pin exactly
//! `public-web | personal | peer` on released records, and the released
//! surfaces in `research/deep-research/notes/custody.md` §5 key on them.
//!
//! `Unknown` is the THIRD VARIANT, never a default: a chunk whose
//! provenance cannot be determined is not quietly `personal` or
//! `public-web` — it is `unknown`, and a factual claim resting on it
//! must refuse (custody.md §4, red R-3). `Unknown` never rides a
//! released record (`is_released_class` is false) — the refusal happens
//! before release, and the gate's per-chunk ledger records
//! `provenance_class: "unknown"` instead.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A chunk's custody class (custody.md §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Custody {
    /// Fetched from the open web (a URL fetch, a crawl, an RSS item, a
    /// published page).
    PublicWeb,
    /// The estate's own material — local files, notes, conversations,
    /// imported personal data.
    Personal,
    /// Material that arrived from another node on the mesh (peer
    /// corpora, shared indexes, gossip-carried content).
    Peer,
    /// Provenance cannot be determined (no stamp, no derivable join).
    /// Refuses — never a default.
    Unknown,
}

impl Custody {
    /// The three released classes. `Unknown` is excluded: a claim resting
    /// on unknown provenance refuses before anything reaches a release
    /// surface (custody.md §4).
    pub const RELEASED_CLASSES: [Custody; 3] =
        [Custody::PublicWeb, Custody::Personal, Custody::Peer];

    /// The exact wire spelling — the one implementation of the contract
    /// strings (the custody reds pin them).
    pub fn as_str(self) -> &'static str {
        match self {
            Custody::PublicWeb => "public-web",
            Custody::Personal => "personal",
            Custody::Peer => "peer",
            Custody::Unknown => "unknown",
        }
    }

    /// True for the three stamped classes; `Unknown` never rides a
    /// released record.
    pub fn is_released_class(self) -> bool {
        !matches!(self, Custody::Unknown)
    }

    /// How restrictive this class is, for release-floor comparison.
    /// `PublicWeb` (0) is the least restrictive — web material egresses
    /// unconditionally; `Personal` (2) the most; `Unknown` (3) sits above
    /// every floor so no floor can release it.
    ///
    /// ONE implementation of the custody ordering (ARCH §10.6). It was a
    /// private `fn restrictiveness` inside `sovereign-core/src/egress.rs`,
    /// which is the third-party-provider boundary; the mesh-peer boundary
    /// needs the same ordering and must not re-derive it. The ordering is a
    /// fact about the classes, so it belongs beside them.
    pub const fn restrictiveness(self) -> u8 {
        match self {
            Custody::PublicWeb => 0,
            Custody::Peer => 1,
            Custody::Personal => 2,
            Custody::Unknown => 3,
        }
    }

    /// Does a release floor of `floor` release content of THIS custody?
    ///
    /// Content releases when it is AT MOST as restrictive as the floor, and
    /// `Unknown` never releases at any floor — a claim resting on
    /// undeterminable provenance refuses before it reaches a release surface
    /// (custody.md §4). The inverse comparison would let a `public-web` floor
    /// release `personal` content, which is the direction that leaks; the
    /// test `a_floor_releases_downward_only` pins it.
    ///
    /// ONE decider, two boundaries (ARCH §10.6): `egress::ConsentGrant::covers`
    /// asks it about a third-party provider, `PeerAnswer::bound_for_peer`
    /// asks it about a mesh peer.
    pub const fn released_by(self, floor: Custody) -> bool {
        !matches!(self, Custody::Unknown) && self.restrictiveness() <= floor.restrictiveness()
    }

    /// Parse a wire spelling (lenient: accepts the exact contract strings
    /// only — a typo is an error, not a silent `Unknown`).
    pub fn parse_wire(s: &str) -> Option<Custody> {
        match s {
            "public-web" => Some(Custody::PublicWeb),
            "personal" => Some(Custody::Personal),
            "peer" => Some(Custody::Peer),
            "unknown" => Some(Custody::Unknown),
            _ => None,
        }
    }
}

impl fmt::Display for Custody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Custody {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Custody::parse_wire(s).ok_or_else(|| format!("unknown custody wire spelling: {s:?}"))
    }
}

/// Max-restrictiveness join over derivation inputs (custody.md §3),
/// computed at creation: `personal > peer > public-web`; any `unknown`
/// input poisons the join to `unknown` (a derived artifact whose inputs
/// include unknown provenance is itself unknown — and refuses).
pub fn join_custody(inputs: &[Custody]) -> Custody {
    let mut joined = Custody::PublicWeb;
    for c in inputs {
        match c {
            Custody::Unknown => return Custody::Unknown,
            Custody::Personal => joined = Custody::Personal,
            Custody::Peer => {
                if joined != Custody::Personal {
                    joined = Custody::Peer;
                }
            }
            Custody::PublicWeb => {}
        }
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_spellings_are_the_contract() {
        assert_eq!(Custody::PublicWeb.as_str(), "public-web");
        assert_eq!(Custody::Personal.as_str(), "personal");
        assert_eq!(Custody::Peer.as_str(), "peer");
        assert_eq!(Custody::Unknown.as_str(), "unknown");
        for c in Custody::RELEASED_CLASSES {
            assert!(c.is_released_class());
        }
        assert!(!Custody::Unknown.is_released_class());
    }

    #[test]
    fn parse_wire_is_exact() {
        assert_eq!(Custody::parse_wire("public-web"), Some(Custody::PublicWeb));
        assert_eq!(Custody::parse_wire("public_web"), None);
        assert_eq!(Custody::parse_wire(""), None);
        assert_eq!("peer".parse::<Custody>().unwrap(), Custody::Peer);
        assert!("PEER".parse::<Custody>().is_err());
    }

    #[test]
    fn a_floor_releases_downward_only() {
        // The direction that leaks is the inverse: a public-web floor must
        // NOT release personal content.
        assert!(Custody::PublicWeb.released_by(Custody::PublicWeb));
        assert!(!Custody::Personal.released_by(Custody::PublicWeb));
        assert!(!Custody::Peer.released_by(Custody::PublicWeb));

        assert!(Custody::PublicWeb.released_by(Custody::Peer));
        assert!(Custody::Peer.released_by(Custody::Peer));
        assert!(!Custody::Personal.released_by(Custody::Peer));

        // `personal` is the most permissive floor and still refuses unknown.
        for c in Custody::RELEASED_CLASSES {
            assert!(
                c.released_by(Custody::Personal),
                "{c} at the personal floor"
            );
        }
        for floor in [
            Custody::PublicWeb,
            Custody::Peer,
            Custody::Personal,
            Custody::Unknown,
        ] {
            assert!(
                !Custody::Unknown.released_by(floor),
                "unknown must refuse at every floor, including {floor}"
            );
        }
    }

    #[test]
    fn the_ordering_matches_the_join() {
        // `join_custody` picks the MOST restrictive input; `restrictiveness`
        // must agree with it or the two deciders disagree about one word.
        let classes = [
            Custody::PublicWeb,
            Custody::Peer,
            Custody::Personal,
            Custody::Unknown,
        ];
        for a in classes {
            for b in classes {
                let joined = join_custody(&[a, b]);
                assert_eq!(
                    joined.restrictiveness(),
                    a.restrictiveness().max(b.restrictiveness()),
                    "join({a}, {b}) = {joined}"
                );
            }
        }
    }

    #[test]
    fn join_is_max_restrictiveness() {
        assert_eq!(
            join_custody(&[Custody::PublicWeb, Custody::PublicWeb]),
            Custody::PublicWeb
        );
        assert_eq!(
            join_custody(&[Custody::Peer, Custody::PublicWeb]),
            Custody::Peer
        );
        assert_eq!(
            join_custody(&[Custody::Personal, Custody::PublicWeb]),
            Custody::Personal
        );
        // personal > peer — order-independent
        assert_eq!(
            join_custody(&[Custody::Peer, Custody::Personal, Custody::PublicWeb]),
            Custody::Personal
        );
        // unknown poisons the join
        assert_eq!(
            join_custody(&[Custody::PublicWeb, Custody::Unknown]),
            Custody::Unknown
        );
    }
}
