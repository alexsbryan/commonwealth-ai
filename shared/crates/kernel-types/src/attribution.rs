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
//! # Two attributions, because a text and a computation fingerprint differently
//!
//! [`Attribution`] answers "which engine wrote this text": model, build,
//! quantization. [`ComputeAttribution`] answers "which machine ran this unit of
//! work": source rev, platform, toolchain. Zero field overlap, because zero
//! shared question — a donor executing a build or a test does not have a model
//! or a quantization, and the engine that answers a question does not have a
//! repo rev. They are siblings here rather than one widened type so that
//! neither can be compared against the other by accident.
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

/// The conditions a unit of work was *computed* under — source rev, platform,
/// toolchain, host.
///
/// Two `ComputeAttribution`s comparing equal is the licence to treat one
/// machine's verdict as if it were your own. That is the whole job: a test that
/// passed on a donor built from a different source rev, on a different OS, or
/// under a different toolchain is not evidence about YOUR tree, and before this
/// type nothing in the system could say so — "a verdict that is not yours" was
/// a convention held in a reviewer's head, not a check.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComputeAttribution {
    /// The source revision the work was computed from — the resolved commit,
    /// never a branch name. A branch is an alias and moves under you, which is
    /// the same defect [`Attribution::model`] records for engines.
    pub repo_rev: String,
    /// The operating system it ran on, as the target triple's second-to-last
    /// component spells it (`linux`, `macos`, `windows`).
    pub os: String,
    /// The CPU architecture it ran on (`x86_64`, `aarch64`).
    pub arch: String,
    /// The toolchain that built it — the compiler identity, not the profile.
    /// A result produced by a different rustc is not a result about the same
    /// program.
    pub toolchain: String,
    /// Which machine computed it. Reuses [`Server`] rather than minting a
    /// second way to say local-or-peer.
    pub host: Server,
}

impl ComputeAttribution {
    /// Whether one machine's result may be read as evidence about the other's
    /// tree. The host is deliberately NOT part of it, for the same reason it is
    /// excluded from [`Attribution::comparable_to`]: accepting a donor's answer
    /// as your own is the entire point of offloading work to the mesh, and a
    /// rule that made `host` count would refuse every donor verdict and leave
    /// the type with nothing to say.
    ///
    /// Everything a donor could plausibly differ on and still be trusted was
    /// considered and none of it survived: the rev, the OS, the arch and the
    /// toolchain each change what the program under test IS, so all four count.
    ///
    /// # A named absence never matches, including another one just like it
    ///
    /// Equality is not enough, because two hosts that BOTH failed to read a
    /// field are not thereby running the same thing. A field naming an absence
    /// ([`crate::is_absent_marker`]) makes this comparison `false` on either
    /// side, so an unreadable rev or toolchain lands as could-not-judge rather
    /// than as agreement (ARCH §18.3: absence is reported, never defaulted).
    ///
    /// THIS USED TO HOLD BY ACCIDENT AND THE ACCIDENT WAS ABOUT TO BE REMOVED.
    /// The rule lived nowhere; what stood in for it was that the work plane's
    /// two `rustc --version` readers spelled their absence differently — the
    /// submitter's said "this checkout's PATH", the donor's said "this donor's
    /// PATH" — so two unreadable hosts compared unequal for the right reason by
    /// luck. Converging those readers onto one spelling is exactly what §10.6
    /// asks for, and doing it would have silently turned "neither of us knows"
    /// into "we agree". Encoded here so it cannot be forgotten by the next
    /// person who tidies the strings (ARCH §7).
    pub fn comparable_to(&self, other: &ComputeAttribution) -> bool {
        let pairs = [
            (&self.repo_rev, &other.repo_rev),
            (&self.os, &other.os),
            (&self.arch, &other.arch),
            (&self.toolchain, &other.toolchain),
        ];
        pairs.iter().all(|(mine, theirs)| {
            mine == theirs && !crate::is_absent_marker(mine) && !crate::is_absent_marker(theirs)
        })
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

    fn compute(repo_rev: &str) -> ComputeAttribution {
        ComputeAttribution {
            repo_rev: repo_rev.into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            toolchain: "1.89.0".into(),
            host: Server::Local,
        }
    }

    #[test]
    fn attribution_from_a_different_rev_is_not_comparable() {
        // The defect this type exists to make impossible: a donor's verdict,
        // produced from a different source tree, read as one of ours.
        assert!(!compute("aaaa111").comparable_to(&compute("bbbb222")));
    }

    #[test]
    fn a_differing_platform_or_toolchain_is_not_comparable() {
        // Keeps the rev test from being the only reject case, so three of the
        // four comparability fields cannot silently drop out of the rule.
        let mine = compute("aaaa111");
        for other in [
            ComputeAttribution {
                os: "macos".into(),
                ..mine.clone()
            },
            ComputeAttribution {
                arch: "aarch64".into(),
                ..mine.clone()
            },
            ComputeAttribution {
                toolchain: "1.90.0".into(),
                ..mine.clone()
            },
        ] {
            assert!(!mine.comparable_to(&other), "{other:?}");
        }
    }

    #[test]
    fn an_identical_attribution_is_comparable() {
        assert!(compute("aaaa111").comparable_to(&compute("aaaa111")));
    }

    #[test]
    fn the_host_is_deliberately_not_part_of_comparability() {
        // The accept case the type exists FOR: a donor ran it, and the verdict
        // is ours to read because everything that defines the program matched.
        let mine = compute("aaaa111");
        let donor = ComputeAttribution {
            host: Server::Peer {
                node: NodeId::from_u128(9),
                name: "halo".into(),
            },
            ..mine.clone()
        };
        assert!(mine.comparable_to(&donor));
        assert_ne!(mine, donor, "host still distinguishes the values");
    }

    /// Two machines that BOTH failed to read the same field are not thereby
    /// running the same thing.
    ///
    /// Failing input: `toolchain` set to one named absence on both sides.
    /// Before this rule, `comparable_to` was plain string equality, so two
    /// identical `"unknown (…)"` markers compared EQUAL and a donor verdict
    /// was adopted as evidence about this tree when neither host could say
    /// which compiler produced it.
    ///
    /// THE OLD SAFETY WAS AN ACCIDENT OF AUTHORSHIP, which is why this is a
    /// rule and not a convention: the submitter's marker read "this
    /// checkout's PATH" and the donor's read "this donor's PATH", so the two
    /// differed and the comparison failed for the right reason by luck. The
    /// moment those two readers converged on one spelling — which is exactly
    /// what §10.6 asks for — the luck would have run out silently.
    #[test]
    fn two_named_absences_in_one_field_are_never_comparable() {
        for field in ["repo_rev", "os", "arch", "toolchain"] {
            let mut a = compute("aaaa111");
            let absent = format!("unknown ({field} could not be read here)");
            assert!(
                crate::is_absent_marker(&absent),
                "the fixture must actually name an absence: {absent}"
            );
            match field {
                "repo_rev" => a.repo_rev = absent.clone(),
                "os" => a.os = absent.clone(),
                "arch" => a.arch = absent.clone(),
                _ => a.toolchain = absent.clone(),
            }
            let b = a.clone();
            assert_eq!(a, b, "the two values are byte-identical");
            assert!(
                !a.comparable_to(&b),
                "{field}: two hosts that both could not read {field} must not \
                 compare as running the same thing"
            );
        }
    }

    /// The accept case stays intact — the rule above must reject absences, not
    /// every comparison. Without this, deleting the whole body of
    /// `comparable_to` and returning `false` would pass the test above.
    #[test]
    fn a_real_value_that_merely_contains_unknown_still_compares() {
        let mut a = compute("aaaa111");
        a.toolchain = "rustc 1.89.0 (unknown-vendor build)".into();
        let b = a.clone();
        assert!(
            !crate::is_absent_marker(&a.toolchain),
            "word-boundary rule: this is a real value, not an absence"
        );
        assert!(a.comparable_to(&b));
    }

    #[test]
    fn compute_attribution_round_trips_on_the_wire() {
        let a = compute("aaaa111");
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<ComputeAttribution>(&j).unwrap(), a);
    }
}
