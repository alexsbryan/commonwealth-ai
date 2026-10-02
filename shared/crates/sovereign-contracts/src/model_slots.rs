// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which of a node's configured models it advertises, by slot: the one
//! decider svrn's slot registration (`/v1/models`, the OICP manifest, the
//! alias map) and serve's servable-file list both read (pb-serve-distributes),
//! so a model peers can route to is a model they can fetch. Beside
//! `setup_config` rather than in it: that file is past its size ceiling.

use std::path::Path;

use crate::setup_config::ModelsSection;

/// `(slot, path)` for every model `[models]` advertises, in registration
/// order: `primary`, `embed`; `fast` only when it is a distinct GGUF (a
/// primary that subsumes the fast role is one chat model, not two); `code`;
/// `primary_<i>` for each copy in a primary pool (the same GGUF, advertised as
/// distinct claims so a scheduler can dispatch round-robin); and
/// `extras:<name>` for each `[models.extra]` slot.
pub fn advertised_slots(models: &ModelsSection) -> Vec<(String, &Path)> {
    let mut slots: Vec<(String, &Path)> = vec![
        ("primary".into(), models.primary.as_path()),
        ("embed".into(), models.embed.as_path()),
    ];
    if models.has_explicit_fast() {
        slots.push(("fast".into(), models.fast_path()));
    }
    if let Some(code_path) = models.code.as_ref() {
        slots.push(("code".into(), code_path.as_path()));
    }
    if let Some(pool) = models.primary_pool.as_ref() {
        for i in 0..pool.copies {
            slots.push((format!("primary_{i}"), pool.path.as_path()));
        }
    }
    for (slot_name, path) in models.extra.iter() {
        slots.push((format!("extras:{slot_name}"), path.as_path()));
    }
    slots
}
