// SPDX-License-Identifier: AGPL-3.0-or-later
//! The NER served kind: named-entity extraction, registered once, loaded once
//! per process, and handed to every reader as the same handle.
//!
//! NER is served IN-PROCESS ONLY. Its route, client method and child role are
//! named absences: every reader today (the NoteStore T2 hook, the tiered
//! chunk adapter, the turn's entity-aware retrieval) runs in the daemon, and
//! a route with no caller is inventory. pb-svrn-dials-serve mints the route
//! when those readers leave the daemon and become its first callers.
//!
//! The kind also owns its asset: which model it loads, whether that model is
//! installed, where it lives and how it is fetched, re-exported below from
//! the one crate that implements them.

use std::sync::{Arc, OnceLock};

use sovereign_contracts::ner::LabeledEntityExtractor;
use sovereign_inference::served_kind::{
    self, KindChild, KindClient, KindLoader, KindRoute, ServedKind,
};

pub use sovereign_gliner::configured_model_id;
pub use sovereign_gliner::gliner_ner::{download_model, models_root, probe_model_available};

/// Why NER has no wire yet; one reason for all three absences.
const IN_PROCESS_ONLY: &str = "NER is served in-process only: no consumer dials it \
     (the NoteStore T2 hook, the tiered chunk adapter and the turn's retrieval \
     all run in the daemon). pb-svrn-dials-serve mints it with its first caller.";

/// The NER kind.
pub const NER: ServedKind = ServedKind {
    role: "ner",
    env_path: None,
    loader: KindLoader::Ner(sovereign_gliner::load_gliner_extractor),
    route: KindRoute::Absent {
        reason: IN_PROCESS_ONLY,
    },
    client: KindClient::Absent {
        reason: IN_PROCESS_ONLY,
    },
    child: KindChild::Absent {
        reason: IN_PROCESS_ONLY,
    },
};

/// This process's one NER handle. The first call registers the kind and
/// loads through its registered loader; every later call, from any reader,
/// gets the same `Arc`. `None` is a node without the model installed (the
/// loader says so at `info`) or a kind that could not register (said here).
pub fn served_ner() -> Option<Arc<dyn LabeledEntityExtractor>> {
    static HANDLE: OnceLock<Option<Arc<dyn LabeledEntityExtractor>>> = OnceLock::new();
    load_once(&HANDLE, NER)
}

/// Register `kind`, then load it through the loader the registry holds for
/// its role, at most once per `cell`.
fn load_once(
    cell: &OnceLock<Option<Arc<dyn LabeledEntityExtractor>>>,
    kind: ServedKind,
) -> Option<Arc<dyn LabeledEntityExtractor>> {
    cell.get_or_init(|| {
        if let Err(e) = served_kind::register_kind(kind) {
            tracing::warn!(target: "served_kind", kind = kind.role, error = %e, "NER kind did not register — no entity extractor this process");
            return None;
        }
        let registered = served_kind::served_kinds()
            .into_iter()
            .find(|k| k.role == kind.role)?;
        match registered.loader {
            KindLoader::Ner(load) => {
                let handle = load();
                tracing::info!(target: "served_kind", kind = kind.role, loaded = handle.is_some(), "NER kind loaded for this process");
                handle
            }
            KindLoader::Provider(_) => {
                tracing::warn!(target: "served_kind", kind = kind.role, "registered kind loads a provider, not an entity extractor");
                None
            }
        }
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_contracts::ner::{EntityMention, GlinerGeneration};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static LOADS: AtomicUsize = AtomicUsize::new(0);

    struct Stub;
    impl LabeledEntityExtractor for Stub {
        fn model_id(&self) -> &str {
            "stub"
        }
        fn labels(&self) -> Vec<String> {
            Vec::new()
        }
        fn threshold(&self) -> f32 {
            0.5
        }
        fn extract_mentions(
            &self,
            _: &str,
        ) -> sovereign_contracts::error::Result<Vec<EntityMention>> {
            Ok(Vec::new())
        }
        fn generation(&self) -> GlinerGeneration {
            GlinerGeneration::V1
        }
    }

    fn stub_loader() -> Option<Arc<dyn LabeledEntityExtractor>> {
        LOADS.fetch_add(1, Ordering::SeqCst);
        Some(Arc::new(Stub))
    }

    /// NER is registered in-process only: its three wire values are named
    /// absences, and it takes no route or child role another kind could clash on.
    #[test]
    fn ner_is_an_in_process_kind_with_named_absences() {
        assert!(matches!(NER.loader, KindLoader::Ner(_)));
        assert_eq!(NER.route_path(), None);
        assert_eq!(NER.child_role(), None);
        assert!(matches!(NER.client, KindClient::Absent { reason } if !reason.is_empty()));
        assert!(NER.load_provider(std::path::Path::new("/x")).is_err());
    }

    /// Two readers asking for the handle get the SAME `Arc`, loaded once,
    /// through the kind's registration.
    #[test]
    fn every_reader_gets_the_one_handle_loaded_once() {
        let cell = OnceLock::new();
        let kind = ServedKind {
            role: "ner-test-once",
            loader: KindLoader::Ner(stub_loader),
            ..NER
        };
        let daemon = load_once(&cell, kind).expect("stub loads");
        let recipe = load_once(&cell, kind).expect("stub loads");
        assert!(Arc::ptr_eq(&daemon, &recipe));
        assert_eq!(LOADS.load(Ordering::SeqCst), 1);
        assert!(served_kind::served_kinds()
            .iter()
            .any(|k| k.role == "ner-test-once"));
    }
}
