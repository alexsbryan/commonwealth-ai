// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a node SERVES to the mesh — the local origins behind its acceptor.
//!
//! Lives in this leaf for the reason `TenantId` does (`tenant.rs`): the
//! roster gossips it (`commonwealth_core::capabilities::NodeCapabilities::
//! origins`), the daemon answers it on `/v1/mesh/status` (`members[].origins`),
//! and a thin client parses that answer. `commonwealth-core` and `sovereign-*`
//! cannot see each other (`quality/ARCH_LAYERS.toml`), so while the enum was
//! defined in commonwealth-core every wire shape carrying it was pinned above
//! the contract layer — `sovereign_mesh::mesh_http::MemberDto` could not move
//! to `sovereign_contracts::daemon_wire` for one closed set of one variant.
//! `oicp-types` is the serde-only crate both already depend on. Moved
//! 2026-09-11 (sv-surface svt-3); `commonwealth_core::capabilities::OriginKind`
//! re-exports it, so no gossip site changed.
//!
//! NOT `kernel-types`: its header splits identity/provenance from federation
//! and it already defines a different `Origin`. This is federation vocabulary.

use serde::{Deserialize, Deserializer, Serialize};

/// A kind of local origin a node can serve to members over the mesh. A
/// closed set (ARCH §2): every kind has one ALPN and one acceptor route, so a
/// new kind is a new variant beside a new route, never a string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// An HTTP media origin (`[iroh] media_origin`), served on `MEDIA_ALPN`.
    Media,
    /// One or more HTTP apps this node publishes by NAME (`[iroh.apps]`),
    /// served on `APP_ALPN` and demultiplexed by a leading path segment.
    ///
    /// The set stays closed while the open set — *which* apps — moves into
    /// config data, which is the rule exactly (ARCH §9: closed sets are
    /// enums, open sets are registries). A housemate writing a chore app at
    /// 1am cannot add an enum variant, an ALPN and an acceptor route and
    /// recompile; they can add a line of config. One variant, one ALPN, one
    /// acceptor route, an unbounded number of apps behind it.
    ///
    /// Its own kind rather than a second `Media`, because it is its own TRUST
    /// class: admitting a housemate to your chore app is not the same
    /// decision as admitting them to your media library, and neither is the
    /// same as admitting them to your tensor RPC.
    App,
}

impl OriginKind {
    /// What to call this kind in a sentence a person reads.
    ///
    /// The viewer refusals name the kind they were ASKED about, and until
    /// 2026-09-12 every one of them said "media origin" because the refusal
    /// enum predated apps and hardcoded the word. So `svrn mesh app <peer>
    /// chores` answered "advertises no media origin" about a node that
    /// advertised one — wrong domain and factually false in the same
    /// sentence, sending the reader to fix a config line already correct.
    ///
    /// One noun per kind, decided here, so the set cannot gain a variant
    /// without answering for how it reads (ARCH §8: one decider, one name).
    pub fn noun(self) -> &'static str {
        match self {
            OriginKind::Media => "media origin",
            OriginKind::App => "published app",
        }
    }

    /// The viewer verb that lists who offers this kind — what to tell someone
    /// whose peer came back empty.
    pub fn viewer_verb(self) -> &'static str {
        match self {
            OriginKind::Media => "svrn mesh media",
            OriginKind::App => "svrn mesh app",
        }
    }

    /// How a HOLDER starts offering this kind, for the other half of that
    /// same refusal: the reader is often the person who has to go fix it.
    pub fn how_to_offer(self) -> &'static str {
        match self {
            OriginKind::Media => "set `[iroh] media_origin`",
            OriginKind::App => "run `svrn publish <name> <port>`",
        }
    }
}

/// Deserialize a gossiped `origins` array, DROPPING kinds this build does not
/// know instead of failing the whole field.
///
/// The failing input, and the reason this exists rather than an
/// `#[serde(other)] Unknown` variant:
///
/// `origins` is `#[serde(default)]`, and serde's `default` applies when a
/// field is ABSENT — not when its value fails to parse. So a peer running an
/// older build that meets `["media","app"]` does not read "one kind I know
/// plus one I don't"; its whole `NodeCapabilities` deserialization errors and
/// the member's entire gossip row is dropped. A house on mixed builds would
/// watch peers vanish from the roster with nothing in the log naming a new
/// origin kind as the cause.
///
/// The tolerance belongs HERE, at the wire boundary where a stranger's bytes
/// arrive, and not in the enum: `OriginKind` is a closed set on purpose and an
/// `Unknown` variant would make every match arm downstream carry a case that
/// means nothing locally. The set stays closed; the boundary absorbs what it
/// cannot name.
///
/// Adding `App` is itself the one break this cannot retroactively prevent —
/// peers already running a build without this function still drop the row. It
/// is the last time the set can do that.
pub fn deserialize_known_origins<'de, D>(d: D) -> Result<Vec<OriginKind>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MaybeKnown {
        Known(OriginKind),
        /// Anything else the wire carried, of ANY shape. `IgnoredAny` rather
        /// than `String` because a future kind need not be a bare string —
        /// gossiped as `{"kind":"future","port":1}` a `String` arm has no
        /// match and the array fails again, which is the whole failure this
        /// function exists to prevent (pinned below).
        Unknown(serde::de::IgnoredAny),
    }
    Ok(Vec::<MaybeKnown>::deserialize(d)?
        .into_iter()
        .filter_map(|m| match m {
            MaybeKnown::Known(k) => Some(k),
            MaybeKnown::Unknown(_) => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gossip bytes are the enum's serde repr, and a rename here is a
    /// roster-wide wire break with nothing red anywhere — so the repr is
    /// pinned, not assumed.
    #[test]
    fn origin_kind_wire_repr_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&OriginKind::Media).unwrap(),
            "\"media\""
        );
        assert_eq!(
            serde_json::from_str::<OriginKind>("\"media\"").unwrap(),
            OriginKind::Media
        );
        assert!(serde_json::from_str::<OriginKind>("\"Media\"").is_err());
        assert_eq!(serde_json::to_string(&OriginKind::App).unwrap(), "\"app\"");
        assert_eq!(
            serde_json::from_str::<OriginKind>("\"app\"").unwrap(),
            OriginKind::App
        );
    }

    #[derive(Debug, Deserialize)]
    struct Row {
        #[serde(default, deserialize_with = "deserialize_known_origins")]
        origins: Vec<OriginKind>,
    }

    /// The failing input the tolerant path exists for: strict parsing errors
    /// on the whole array, which upstream turns into a dropped roster row.
    #[test]
    fn a_strict_parse_of_an_unknown_kind_fails_the_whole_array() {
        assert!(serde_json::from_str::<Vec<OriginKind>>(r#"["media","quantum"]"#).is_err());
    }

    #[test]
    fn an_unknown_kind_is_dropped_and_the_known_ones_survive() {
        let r: Row = serde_json::from_str(r#"{"origins":["media","quantum","app"]}"#).unwrap();
        assert_eq!(r.origins, vec![OriginKind::Media, OriginKind::App]);
    }

    /// Absence still reads as "advertises none", and an all-unknown array
    /// reads the same way — never as an offer this build cannot serve.
    #[test]
    fn absence_and_all_unknown_both_read_as_no_offer() {
        let absent: Row = serde_json::from_str("{}").unwrap();
        assert!(absent.origins.is_empty());
        let unknown: Row = serde_json::from_str(r#"{"origins":["quantum"]}"#).unwrap();
        assert!(unknown.origins.is_empty());
    }

    /// A shape that is not a string at all (a future kind gossiped as an
    /// object) must also be skipped rather than fail the row.
    #[test]
    fn a_non_string_kind_is_skipped_too() {
        let r: Row =
            serde_json::from_str(r#"{"origins":["media",{"kind":"future","port":1}]}"#).unwrap();
        assert_eq!(r.origins, vec![OriginKind::Media]);
    }
}
