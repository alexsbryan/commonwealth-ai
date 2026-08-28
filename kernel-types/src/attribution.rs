// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Attribution`] — which engine computed a piece of text.
//!
//! # One answer, four consumers
//!
//! Four shapes described this fact before the type existed: a byte-duplicated
//! `ModelInfo` pair, an unrelated `/v1/models` mirror under the same name, a
//! private capability `Provenance` enum, and a pinned judge model living in a
//! doc comment as the comparability guarantee. The fleet's worst attribution
//! incident — the fast-slot alias hijack, ARCH §10.6's first exhibit — is this
//! noun's absence, measured.
//!
//! It belongs at layer 0 rather than in `sovereign` because the consumers span
//! every domain: a measurement's fingerprint, a judgement's register, an
//! answer's provenance, and what a peer advertises over OICP all need to say
//! WHICH engine produced a piece of text, and they must all say it the same
//! way or two numbers that are not comparable will be compared.
//!
//! # This name was contested, and the other holder was renamed
//!
//! `corpus-engine` had an `Attribution` meaning the SPEAKER of a chat turn
//! (`User | Assistant | Unattributed | Pasted`). Same word, different concept.
//! That one became `TurnAuthor`, which is what it always was; this keeps the
//! bare name because it is the published cross-domain one and the register
//! assigns the noun to the kernel.

use serde::{Deserialize, Serialize};

use crate::Server;

/// Which engine computed a piece of text — model, build, quantization, host.
///
/// Two `Attribution`s comparing equal is the licence to compare the numbers
/// they label. That is the whole job: a benchmark delta across two different
/// quantizations of nominally the same model is not a delta, and before this
/// type nothing in the system could say so.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Attribution {
    /// The model as actually served — the resolved identity, never an alias.
    /// Recording the alias is precisely how the fast-slot hijack went
    /// unnoticed: two different models answered to one name.
    pub model: String,
    /// The engine build that ran it.
    pub build: String,
    /// The quantization, when the weights are quantized. `None` means
    /// genuinely unquantized — reported absence, not a `"none"` sentinel that
    /// a future reader has to know is special (principle 6).
    pub quantization: Option<String>,
    /// Which machine computed it. Reuses [`Server`] rather than minting a
    /// second way to say local-or-peer.
    pub host: Server,
}

impl Attribution {
    /// Whether two pieces of text were produced under conditions comparable
    /// enough to diff. The host is deliberately NOT part of it: the same
    /// model, build and quantization on two machines is the comparison the
    /// mesh exists to make.
    pub fn comparable_to(&self, other: &Attribution) -> bool {
        self.model == other.model
            && self.build == other.build
            && self.quantization == other.quantization
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;

    fn attr(model: &str, quant: Option<&str>) -> Attribution {
        Attribution {
            model: model.into(),
            build: "b1".into(),
            quantization: quant.map(Into::into),
            host: Server::Local,
        }
    }

    #[test]
    fn two_quantizations_of_one_model_are_not_comparable() {
        // The defect this type exists to make impossible: a bench delta
        // across quantizations read as a delta in the thing being measured.
        assert!(!attr("m", Some("Q4_K_M")).comparable_to(&attr("m", Some("Q8_0"))));
        assert!(!attr("m", Some("Q4_K_M")).comparable_to(&attr("m", None)));
    }

    #[test]
    fn the_same_engine_on_two_machines_is_comparable() {
        let local = attr("m", Some("Q4_K_M"));
        let peer = Attribution {
            host: Server::Peer {
                node: NodeId::from_u128(9),
                name: "halo".into(),
            },
            ..local.clone()
        };
        assert!(local.comparable_to(&peer));
        assert_ne!(local, peer, "host still distinguishes the values");
    }

    #[test]
    fn unquantized_is_none_not_a_sentinel_string() {
        let a = attr("m", None);
        assert_eq!(a.quantization, None);
        let j = serde_json::to_string(&a).unwrap();
        assert!(j.contains("\"quantization\":null"), "{j}");
    }

    #[test]
    fn round_trips_on_the_wire() {
        let a = attr("m", Some("Q4_K_M"));
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<Attribution>(&j).unwrap(), a);
    }
}
