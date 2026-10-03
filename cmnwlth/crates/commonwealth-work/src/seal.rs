// SPDX-License-Identifier: AGPL-3.0-or-later
//! The unit seal — a [`JobUnit`]'s identity, computed once, here.
//!
//! `JobUnit::unit_hash` is a **field** in `oicp-types` and a **computation**
//! in this crate, and the split is deliberate: the leaf cannot name
//! [`Payload`], and `Payload::new`
//! (`commonwealth-rail-core/src/payload.rs`) is the one canonicalizer in this
//! tree. A leaf that re-derived its rules would disagree with it exactly when
//! it mattered — a fractional number, a key order, a 64 KiB boundary — and the
//! disagreement would surface as two units with different identities for one
//! payload, on two nodes, with nothing red.
//!
//! # Where this lives, and why not in `commonwealth-core`
//!
//! `commonwealth-core` has no `commonwealth-rail-core` dependency, so it
//! cannot reach `Payload` at all; and the reverse edge is forbidden outright
//! (`quality/ARCH_LAYERS.toml`, the `commonwealth-rail* -> commonwealth-*`
//! block — "a rail is not a mesh"). Both were checked before this module was
//! placed. `commonwealth-work` depends on both, which makes it the only home
//! in the package where the seal can be written at all.
//!
//! # The preimage
//!
//! ```text
//! unit_hash = ContentHash::of( bytes of Payload::new({ "kind": "<id:vN>", "payload": <body> }) )
//! ```
//!
//! Two fields and no more. `requirements` and `tenant` are deliberately
//! outside it: they say where a unit may run and as whom, not what it IS, and
//! folding them in would mean the same command pinned to two different OSes
//! was two units — which would defeat the idempotency the hash exists to give
//! (`at-least-once, idempotent per unit_hash`).
//!
//! `kind` is inside it, and
//! `unit_hash_differs_for_same_payload_under_different_kind` is why: the same
//! `{"argv": [...]}` body means one thing to `process:v1` and something else
//! to a later `process:v2` or to `ingest:v1`. A hash over the body alone would
//! call those one unit and let a donor complete the wrong one.
//!
//! Note the two different `kind`s in this crate and do not confuse them: the
//! one here is the JOB kind (`process:v1`), and the one on an act payload is
//! the ACT kind (`work.submit`) — see [`crate::act`].

use commonwealth_rail_core::{Payload, PayloadError};
use kernel_types::ContentHash;
use oicp_types::{JobKind, JobRequirements, JobUnit, TenantId};
use serde_json::Value;

/// Why a unit has no valid identity. Both variants render as sentences: they
/// reach an operator through `svrn job submit`'s refusal and through the rail's
/// own 422 body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkSealError {
    /// The payload cannot be put on the rail at all, so it cannot be hashed
    /// with the rail's canonicalizer. Carries the rail's own sentence, which
    /// already names the fix.
    ///
    /// This is the **named refusal**, not a truncation and not a rounding: a
    /// `timeout_secs` of `1.5` never becomes `1` or `2` on its way to a donor
    /// (ARCH §18.3).
    #[error("this unit has no identity, because its payload cannot go on the rail: {0}")]
    NotCanonical(#[from] PayloadError),
    /// The unit's declared hash is not the hash of its own payload. Either the
    /// payload was edited after sealing, or the unit came from a peer that
    /// computes identity some other way.
    #[error(
        "this unit declares the identity {declared} but its own payload seals as {computed} — \
         a unit whose hash does not cover its payload cannot be deduplicated, leased or completed \
         idempotently, which is what the hash is for"
    )]
    Mismatch { declared: String, computed: String },
}

/// **The** unit-identity computation. Every other site reaches this one.
///
/// `kind` is rendered in its one wire spelling (`id:vN`, from
/// [`JobKind`]'s `Display`), never re-spelled here.
pub fn unit_hash(kind: &JobKind, payload: &Value) -> Result<String, WorkSealError> {
    let preimage = Payload::new(serde_json::json!({
        "kind": kind.to_string(),
        "payload": payload,
    }))?;
    // `Display for serde_json::Value` is serde_json's own compact writer — the
    // same bytes `Payload`'s `Serialize` produces, since that impl just
    // forwards to the inner `Value`. Taken through `Display` rather than
    // `to_vec` because it is INFALLIBLE: a fallible call here would need
    // either an `expect` (a panic on a path that cannot happen) or a
    // `WorkSealError` variant nothing can ever construct, and both are worse
    // than saying so here.
    let bytes = preimage.as_value().to_string();
    Ok(ContentHash::of(bytes.as_bytes()).to_hex())
}

/// Mint a sealed [`JobUnit`] — the only door in this crate that produces one.
///
/// Takes the fields rather than an unsealed `JobUnit` on purpose: `JobUnit`
/// has public fields in a leaf that cannot compute the hash, so a
/// `seal(&mut unit)` signature would mean an unsealed unit is a representable
/// value people pass around. This way the type in hand is either sealed or was
/// not built here.
pub fn seal(
    kind: JobKind,
    payload: Value,
    requirements: JobRequirements,
    tenant: Option<TenantId>,
) -> Result<JobUnit, WorkSealError> {
    let unit_hash = unit_hash(&kind, &payload)?;
    Ok(JobUnit {
        kind,
        unit_hash,
        payload,
        requirements,
        tenant,
    })
}

/// Re-derive a unit's identity and check it against what the unit claims.
///
/// Run on every unit that arrives from a peer, before it is queued: a unit is
/// deduplicated, leased and completed by `unit_hash`, so a hash that does not
/// cover the payload is a donor running one thing and reporting another.
pub fn verify(unit: &JobUnit) -> Result<(), WorkSealError> {
    let computed = unit_hash(&unit.kind, &unit.payload)?;
    if computed == unit.unit_hash {
        return Ok(());
    }
    tracing::debug!(
        target: crate::TRACE_TARGET,
        declared = %unit.unit_hash,
        computed = %computed,
        kind = %unit.kind,
        "unit seal mismatch"
    );
    Err(WorkSealError::Mismatch {
        declared: unit.unit_hash.clone(),
        computed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kind(raw: &str) -> JobKind {
        JobKind::parse(raw).expect("test kind")
    }

    /// **The named test.** Failing input: the identical body
    /// `{"argv":["cargo","test"]}` under `process:v1` and under `ingest:v1`.
    /// With `kind` outside the preimage these are one identity, and a donor
    /// offering only `ingest:v1` could lease and complete the `process:v1`
    /// unit, reporting a verdict for work it never ran.
    #[test]
    fn unit_hash_differs_for_same_payload_under_different_kind() {
        let body = json!({ "argv": ["cargo", "test"] });
        let a = unit_hash(&kind("process:v1"), &body).unwrap();
        let b = unit_hash(&kind("ingest:v1"), &body).unwrap();
        let c = unit_hash(&kind("process:v2"), &body).unwrap();

        assert_ne!(a, b, "process:v1 and ingest:v1 must not be one unit");
        assert_ne!(a, c, "a version bump changes what the body means");
        assert_ne!(b, c);
        // And the hash is still a function of the body alone within one kind.
        assert_eq!(a, unit_hash(&kind("process:v1"), &body).unwrap());
    }

    /// **The named test.** Failing input: `timeout_secs: 1.5`.
    ///
    /// The two wrong answers this rules out are a TRUNCATION (`1`) and a
    /// ROUNDING (`2`): either would hand a donor a different command than the
    /// submitter wrote, with a hash that looks perfectly valid. The right
    /// answer is a named refusal that says what to write instead.
    #[test]
    fn a_fractional_number_in_a_payload_is_a_named_refusal_not_a_truncation() {
        let body = json!({ "argv": ["sleep", "2"], "timeout_secs": 1.5 });

        let err = unit_hash(&kind("process:v1"), &body).unwrap_err();
        assert_eq!(
            err,
            WorkSealError::NotCanonical(PayloadError::Fractional("1.5".to_string())),
            "the refusal must name the fractional value, not swallow it"
        );

        let sentence = err.to_string();
        assert!(sentence.contains("1.5"), "{sentence}");
        assert!(sentence.contains("whole number"), "{sentence}");

        // No truncation, no rounding: the two integer spellings that a
        // silently-coercing implementation would have produced are NOT what
        // this payload seals as. (They are legal payloads on their own — which
        // is exactly why accepting 1.5 as one of them would be invisible.)
        let truncated = unit_hash(
            &kind("process:v1"),
            &json!({ "argv": ["sleep", "2"], "timeout_secs": 1 }),
        )
        .unwrap();
        let rounded = unit_hash(
            &kind("process:v1"),
            &json!({ "argv": ["sleep", "2"], "timeout_secs": 2 }),
        )
        .unwrap();
        assert_ne!(truncated, rounded);
        assert!(seal(kind("process:v1"), body, JobRequirements::any(), None).is_err());
    }

    /// Two spellings of one act are one unit — the property `Payload` exists
    /// for, checked at the seal because that is where it becomes an identity.
    #[test]
    fn key_order_does_not_change_a_units_identity() {
        let a = unit_hash(&kind("process:v1"), &json!({ "b": 1, "a": 2 })).unwrap();
        let b = unit_hash(&kind("process:v1"), &json!({ "a": 2, "b": 1 })).unwrap();
        assert_eq!(a, b);
    }

    /// Requirements are outside the preimage on purpose: the same command
    /// pinned to two OSes is one unit, run twice, not two units.
    #[test]
    fn requirements_are_not_part_of_the_identity() {
        let body = json!({ "argv": ["uname", "-a"] });
        let bare = seal(
            kind("process:v1"),
            body.clone(),
            JobRequirements::any(),
            None,
        )
        .unwrap();
        let pinned = seal(
            kind("process:v1"),
            body,
            JobRequirements {
                os: Some("linux".into()),
                ..JobRequirements::any()
            },
            None,
        )
        .unwrap();
        assert_eq!(bare.unit_hash, pinned.unit_hash);
    }

    #[test]
    fn verify_accepts_what_seal_minted() {
        let unit = seal(
            kind("process:v1"),
            json!({ "argv": ["true"] }),
            JobRequirements::any(),
            None,
        )
        .unwrap();
        assert_eq!(verify(&unit), Ok(()));
    }

    /// Failing input: a sealed unit whose body is edited afterwards — the
    /// shape a tampering peer, or a careless caller, actually produces.
    #[test]
    fn verify_refuses_a_unit_whose_payload_was_edited_after_sealing() {
        let mut unit = seal(
            kind("process:v1"),
            json!({ "argv": ["true"] }),
            JobRequirements::any(),
            None,
        )
        .unwrap();
        let declared = unit.unit_hash.clone();
        unit.payload = json!({ "argv": ["rm", "-rf", "/"] });

        let err = verify(&unit).unwrap_err();
        match err {
            WorkSealError::Mismatch {
                declared: d,
                computed,
            } => {
                assert_eq!(d, declared);
                assert_ne!(computed, declared);
            }
            other => panic!("expected a mismatch, got {other:?}"),
        }
    }

    /// A payload over the rail's cap is refused, never truncated to fit.
    #[test]
    fn an_oversized_payload_is_refused_rather_than_truncated() {
        let big = "x".repeat(70 * 1024);
        let err = unit_hash(&kind("process:v1"), &json!({ "stdin": big })).unwrap_err();
        assert!(
            matches!(err, WorkSealError::NotCanonical(PayloadError::TooLarge(_))),
            "{err:?}"
        );
    }
}
