// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a node SERVES to the mesh — the local origins behind its acceptor.
//!
//! Lives in this leaf for the reason `TenantId` does (`tenant.rs`): the
//! roster gossips it (`commonwealth_core::capabilities::NodeCapabilities::
//! origins`), the daemon answers it on `/v1/mesh/status` (`members[].origins`),
//! and a thin client parses that answer. `commonwealth-core` and `sovereign-*`
//! cannot see each other (`quality/ARCH_LAYERS.toml`), so while the enum was
//! defined in commonwealth-core every wire shape carrying it was pinned above
//! the contract layer — `sovereign_daemon::mesh_http::MemberDto` could not move
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
    /// An HTTP origin listing what this node's operator has to SELL or LEND
    /// (`[iroh] offer_origin`), served on `OFFER_ALPN`.
    ///
    /// The catalogue of a mesh marketplace is COMPUTED, never stored: there
    /// is no listing to be excluded from and no ranking, because nobody is
    /// positioned to rank. `origin_fanout` asks every member that publishes
    /// one the same question and returns a row each — including a
    /// `never_asked` row for the member that publishes none, which is the
    /// property no marketplace has (`docs/internal/RING_APPLICATIONS.md`
    /// §Commerce).
    ///
    /// **What rides behind it is the ORIGIN's, entirely.** This kind carries
    /// no item schema and the substrate merges and dedups nothing — that
    /// refusal is `commonwealth_media::fanout`'s and it is the reason a
    /// marketplace can be a second fold consumer rather than a fork. What
    /// "an offer" means is a question for a shim that speaks one seller's
    /// catalogue; this enum only says which ALPN reaches it.
    ///
    /// **Its own kind rather than an `App` named `offers`, for the reason
    /// `App` is not a path on `Media`: it is its own TRUST class.** Letting
    /// the house see what you have for sale is a different decision from
    /// letting them into your chore app, and a house that cannot express the
    /// narrower grant says yes to everything instead. `[iroh] offer_allow`
    /// is that expression, and it is a separate list.
    Offer,
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
            // NOT "offer" or "catalogue". A refusal reads "'Jonas' advertises
            // no <noun>", and the thing Jonas failed to advertise is the
            // ORIGIN — the HTTP server behind `[iroh] offer_origin` — not the
            // individual offers, which this repository never sees and has no
            // business counting. "advertises no offers" would send the reader
            // to look for an empty catalogue that does not exist.
            OriginKind::Offer => "offer origin",
        }
    }

    /// The viewer verb that lists who offers this kind — what to tell someone
    /// whose peer came back empty.
    pub fn viewer_verb(self) -> &'static str {
        match self {
            OriginKind::Media => "svrn mesh media",
            OriginKind::App => "svrn mesh app",
            // `--who` and not the bare verb, unlike the other two. Bare
            // `svrn mesh offers` is the CATALOGUE — it asks, and its answer
            // already carries this refusal as a row. Sending a reader who
            // just read that row back to the command that printed it is a
            // loop; `--who` is the gossip list, which is what "who offers
            // one" means here.
            OriginKind::Offer => "svrn mesh offers --who",
        }
    }

    /// How a HOLDER starts offering this kind, for the other half of that
    /// same refusal: the reader is often the person who has to go fix it.
    pub fn how_to_offer(self) -> &'static str {
        match self {
            OriginKind::Media => "set `[iroh] media_origin`",
            OriginKind::App => "run `svrn publish <name> <port>`",
            // Config, not a verb: unlike an app, an offer origin is a server
            // the operator already runs and wants up permanently, so the
            // durable tier is the right one and there is no ephemeral
            // claim tier for it in this rung.
            OriginKind::Offer => "set `[iroh] offer_origin`",
        }
    }

    /// Every kind, for a surface that must enumerate the closed set — the
    /// refusal that names what a build DOES know when it meets a kind it
    /// does not (see `sovereign_daemon::origin_fanout`).
    ///
    /// A const array rather than a `strum` derive or a hand-written list at
    /// the call site: a fourth variant that forgets to appear here is a
    /// refusal that lies about this build's own vocabulary.
    pub const ALL: [OriginKind; 3] = [OriginKind::Media, OriginKind::App, OriginKind::Offer];

    /// The wire spelling — the serde repr, as one function rather than as a
    /// `serde_json::to_string` at each call site that wants to PRINT a kind.
    pub fn wire(self) -> &'static str {
        match self {
            OriginKind::Media => "media",
            OriginKind::App => "app",
            OriginKind::Offer => "offer",
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
///
/// `Offer` (2026-09-13) is the first kind that tolerance actually covers: a
/// peer carrying this function meets `["media","offer"]`, keeps `media`, and
/// stays on the roster. What it does NOT get is a way to say so — the dropped
/// kind leaves no trace in `NodeCapabilities`, so a build that knows `offer`
/// cannot tell "this peer runs an older build" from "this peer publishes no
/// offer origin" by reading gossip. That distinction is made where a kind is
/// ASKED for by name and the answering build has no name for it
/// (`commonwealth_media::fanout::AskedKind`), not here; the boundary's job is
/// to keep the row, and it does.
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
        assert_eq!(
            serde_json::to_string(&OriginKind::Offer).unwrap(),
            "\"offer\""
        );
        assert_eq!(
            serde_json::from_str::<OriginKind>("\"offer\"").unwrap(),
            OriginKind::Offer
        );
    }

    /// [`OriginKind::wire`] is a SECOND spelling of the serde repr, so it is
    /// pinned to the first rather than trusted. The failing input is a
    /// variant whose `wire()` drifts from its `rename_all` form — which a
    /// refusal would then print as a kind no peer would accept.
    #[test]
    fn the_printable_wire_name_is_the_serde_name() {
        for kind in OriginKind::ALL {
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.wire()),
                "{kind:?}"
            );
        }
    }

    /// Every variant is in `ALL`. Not provable by construction, so it is
    /// proved by round-tripping every wire name back through serde and
    /// counting: a fourth variant missing from `ALL` makes a refusal
    /// understate this build's own vocabulary.
    #[test]
    fn all_names_every_variant() {
        let mut seen: Vec<&str> = OriginKind::ALL.iter().map(|k| k.wire()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), OriginKind::ALL.len(), "duplicate in ALL");
        assert_eq!(seen, vec!["app", "media", "offer"]);
    }

    /// The three sentence fragments are per-variant so a refusal cannot
    /// describe the wrong domain (the 2026-09-12 defect where every app
    /// refusal said "media origin"). The failing input is a new variant
    /// whose arms were copied from an old one.
    #[test]
    fn every_kind_names_itself_and_no_other() {
        for kind in OriginKind::ALL {
            for other in OriginKind::ALL {
                if kind == other {
                    continue;
                }
                assert_ne!(kind.noun(), other.noun(), "{kind:?} vs {other:?}");
                assert_ne!(
                    kind.how_to_offer(),
                    other.how_to_offer(),
                    "{kind:?} vs {other:?}"
                );
                assert_ne!(
                    kind.viewer_verb(),
                    other.viewer_verb(),
                    "{kind:?} vs {other:?}"
                );
            }
        }
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
