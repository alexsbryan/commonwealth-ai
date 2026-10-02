// SPDX-License-Identifier: AGPL-3.0-or-later
//! The projection's wire form — what `cw-rails`' `GET /v1/work/projection`
//! serves and a donor reads (five-programs fp-45: the fold runs where the
//! journal is, and a reader receives the folded queue rather than folding).

use std::collections::BTreeMap;

use kernel_types::HandoffId;
use serde::{Deserialize, Deserializer, Serializer};

use super::WorkHandoff;

/// `HandoffId` serialises as its 16 bytes, which JSON cannot use as an
/// object key, so the handoff map travels as a list of `(id, handoff)` pairs.
pub(super) mod handoffs_as_pairs {
    use super::*;

    pub fn serialize<S: Serializer>(
        m: &BTreeMap<HandoffId, WorkHandoff>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_seq(m.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<HandoffId, WorkHandoff>, D::Error> {
        Ok(Vec::<(HandoffId, WorkHandoff)>::deserialize(d)?
            .into_iter()
            .collect())
    }
}
