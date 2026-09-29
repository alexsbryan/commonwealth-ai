// SPDX-License-Identifier: AGPL-3.0-or-later
//! Served model kinds — what a model that is not a chat or embed slot needs
//! from a serving process, declared once.
//!
//! [`crate::engine_factory::register_engine`] answers "which engine?". This
//! answers "which extra kinds of model does that engine serve, and how is each
//! reached?". A kind registers five things together: its role (the
//! `[models.kinds]` key and the name it is logged under), its loader, its
//! OICP route, the client method that dials that route, and the compute-child
//! role that hosts it out of process. The route mount, the compute child and
//! the model-path decider all read this table, so a new kind is one
//! [`ServedKind`] value, not an edit at each of those sites.
//!
//! The route, the client method and the child role may each be a NAMED
//! absence: an enum arm carrying the reason, never a silent `None` (ARCH §6).
//! A kind served in-process only says why it has no route.
//!
//! Slot roles (fast, primary, embed, code) are not kinds and stay a closed
//! enum (`SLOT_ALIAS_POLICY`, ARCH §9).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use futures::future::BoxFuture;
use sovereign_contracts::ner::LabeledEntityExtractor;
use sovereign_contracts::rerank_kind::RERANK_ROLE;
use sovereign_contracts::setup_config::ModelsSection;
use sovereign_contracts::traits::InferenceProvider;

/// How a kind loads its model. A closed set, one arm per handle shape a
/// serving process holds (ARCH §9).
#[derive(Clone, Copy)]
pub enum KindLoader {
    /// A GGUF path, loaded into a provider that serves it.
    Provider(fn(&Path) -> sovereign_contracts::error::Result<Arc<dyn InferenceProvider>>),
    /// A named-entity extractor. It resolves its own model (an id, not a GGUF
    /// path); `None` is a model this node has not installed, which the loader
    /// reports itself.
    Ner(fn() -> Option<Arc<dyn LabeledEntityExtractor>>),
}

/// Serves one request on a kind's route: the JSON body in, the JSON answer
/// out, against the serving process's provider. Transport-free, so the route
/// mount owns HTTP and this crate stays free of it.
pub type KindServeFn = fn(
    Arc<dyn InferenceProvider>,
    serde_json::Value,
) -> BoxFuture<'static, Result<serde_json::Value, KindServeError>>;

/// Why a kind's route refused a request. The mount maps each arm to a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindServeError {
    /// The body is not this kind's request shape (400).
    BadRequest(String),
    /// The provider could not serve it (503).
    Backend(String),
}

/// The OICP route a kind is served on.
#[derive(Clone, Copy)]
pub enum KindRoute {
    /// Mounted at `path`; `serve` answers it.
    Served {
        /// The route path, e.g. `/v1/rerank`.
        path: &'static str,
        /// The handler body.
        serve: KindServeFn,
    },
    /// No route, and why.
    Absent {
        /// The reason, for the reader who expected one.
        reason: &'static str,
    },
}

/// The `InferenceProvider` method a remote client (`oicp-client`) implements
/// by dialling the kind's route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindClient {
    /// Implemented as this trait method.
    Provided {
        /// The `InferenceProvider` method name.
        method: &'static str,
    },
    /// No client method, and why.
    Absent {
        /// The reason.
        reason: &'static str,
    },
}

/// The compute-child role that hosts a kind out of process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindChild {
    /// `--role <role>` / `[[compute.slot]] role = "<role>"` loads the kind's
    /// model through its [`KindLoader`] in a supervised child.
    Hosted {
        /// The child role name.
        role: &'static str,
    },
    /// No child role, and why.
    Absent {
        /// The reason.
        reason: &'static str,
    },
}

/// One served model kind.
#[derive(Clone, Copy)]
pub struct ServedKind {
    /// The kind's name: its `[models.kinds]` key and its log label.
    pub role: &'static str,
    /// An env var that names the GGUF and wins over the config key, for the
    /// kinds that had one before `[models.kinds]` existed. `None` for a kind
    /// with no such variable.
    pub env_path: Option<&'static str>,
    /// Loads the kind's model into a provider that serves it.
    pub loader: KindLoader,
    /// Its OICP route.
    pub route: KindRoute,
    /// Its client method.
    pub client: KindClient,
    /// Its compute-child role.
    pub child: KindChild,
}

impl std::fmt::Debug for ServedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let route = match self.route {
            KindRoute::Served { path, .. } => path,
            KindRoute::Absent { reason } => reason,
        };
        f.debug_struct("ServedKind")
            .field("role", &self.role)
            .field("env_path", &self.env_path)
            .field("route", &route)
            .field("client", &self.client)
            .field("child", &self.child)
            .finish()
    }
}

impl ServedKind {
    /// The GGUF this kind loads on a node with `models`, if any: the env var
    /// when it is set, else the `[models.kinds]` key. The one decider every
    /// load site reads.
    pub fn model_path(&self, models: Option<&ModelsSection>) -> Option<PathBuf> {
        let from_env = self.env_path.and_then(|name| std::env::var(name).ok());
        let path = from_env
            .map(PathBuf::from)
            .or_else(|| models.and_then(|m| m.kinds.get(self.role).cloned()));
        tracing::debug!(
            target: "served_kind",
            kind = self.role,
            path = ?path,
            "served kind model path resolved"
        );
        path
    }

    /// The route path, when the kind has one.
    pub fn route_path(&self) -> Option<&'static str> {
        match self.route {
            KindRoute::Served { path, .. } => Some(path),
            KindRoute::Absent { .. } => None,
        }
    }

    /// The child role, when the kind has one.
    pub fn child_role(&self) -> Option<&'static str> {
        match self.child {
            KindChild::Hosted { role } => Some(role),
            KindChild::Absent { .. } => None,
        }
    }

    /// Load this kind's GGUF at `path` into a provider. A kind whose loader
    /// is not a provider refuses by name rather than loading nothing.
    pub fn load_provider(
        &self,
        path: &Path,
    ) -> sovereign_contracts::error::Result<Arc<dyn InferenceProvider>> {
        match self.loader {
            KindLoader::Provider(load) => load(path),
            KindLoader::Ner(_) => Err(sovereign_contracts::error::Error::Inference(format!(
                "served kind `{}` loads an entity extractor, not a provider",
                self.role
            ))),
        }
    }
}

/// The cross-encoder reranker.
pub const RERANK: ServedKind = ServedKind {
    role: RERANK_ROLE,
    env_path: Some("SOVEREIGN_RERANK_MODEL_PATH"),
    loader: KindLoader::Provider(load_rerank),
    route: KindRoute::Served {
        path: "/v1/rerank",
        serve: serve_rerank,
    },
    client: KindClient::Provided {
        method: "rerank_batch",
    },
    child: KindChild::Hosted { role: RERANK_ROLE },
};

/// The one in-process rerank load, fit check first
/// (`reranker_standalone::load_fitted`). A refusal and a failed load are both
/// an `Err` naming which one it was.
fn load_rerank(path: &Path) -> sovereign_contracts::error::Result<Arc<dyn InferenceProvider>> {
    use crate::reranker_standalone::RerankLoad;
    match crate::reranker_standalone::load_fitted(path) {
        RerankLoad::Loaded(provider) => Ok(provider),
        RerankLoad::Refused { message } | RerankLoad::Failed { message } => {
            Err(sovereign_contracts::error::Error::Inference(message))
        }
        RerankLoad::NotConfigured => Err(sovereign_contracts::error::Error::Inference(format!(
            "no reranker configured at {}",
            path.display()
        ))),
    }
}

fn serve_rerank(
    provider: Arc<dyn InferenceProvider>,
    body: serde_json::Value,
) -> BoxFuture<'static, Result<serde_json::Value, KindServeError>> {
    Box::pin(async move {
        let request: oicp_types::openai_types::RerankRequest = serde_json::from_value(body)
            .map_err(|e| KindServeError::BadRequest(format!("rerank request: {e}")))?;
        if request.documents.is_empty() {
            return Err(KindServeError::BadRequest(
                "rerank request: `documents` must be a non-empty array".to_string(),
            ));
        }
        let scores = provider
            .rerank_batch(&request.query, &request.documents)
            .await
            .map_err(|e| KindServeError::Backend(format!("rerank failed: {e}")))?;
        if scores.len() != request.documents.len() {
            return Err(KindServeError::Backend(format!(
                "rerank backend returned {} scores for {} documents",
                scores.len(),
                request.documents.len()
            )));
        }
        let response = oicp_types::openai_types::RerankResponse {
            model: request.model,
            results: scores
                .into_iter()
                .enumerate()
                .map(
                    |(index, relevance_score)| oicp_types::openai_types::RerankResult {
                        index,
                        relevance_score,
                    },
                )
                .collect(),
        };
        serde_json::to_value(response).map_err(|e| KindServeError::Backend(e.to_string()))
    })
}

type Registry = RwLock<Vec<ServedKind>>;

/// The kinds every serving binary registers, admitted through the same clash
/// check as an out-of-tree [`register_kind`].
const BUILT_IN: [ServedKind; 1] = [RERANK];

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut kinds = Vec::new();
        for kind in BUILT_IN {
            if let Err(e) = admit(&mut kinds, kind) {
                panic!("the built-in served kinds clash: {e}");
            }
        }
        RwLock::new(kinds)
    })
}

/// Register an out-of-tree kind. Call before the host mounts its routes.
///
/// Refuses a kind whose role, route path or child role is already taken: two
/// kinds behind one name is two deciders, and the one that wins would be
/// chosen by registration order.
pub fn register_kind(kind: ServedKind) -> Result<(), String> {
    let mut guard = registry()
        .write()
        .map_err(|_| "served kind registry poisoned".to_string())?;
    admit(&mut guard, kind)
}

/// Add `kind` to `kinds` unless its role, route or child role is taken.
fn admit(kinds: &mut Vec<ServedKind>, kind: ServedKind) -> Result<(), String> {
    for existing in kinds.iter() {
        if existing.role == kind.role {
            return Err(format!("served kind `{}` is already registered", kind.role));
        }
        if let (Some(a), Some(b)) = (existing.route_path(), kind.route_path()) {
            if a == b {
                return Err(format!(
                    "route `{a}` already serves kind `{}`; `{}` cannot take it",
                    existing.role, kind.role
                ));
            }
        }
        if let (Some(a), Some(b)) = (existing.child_role(), kind.child_role()) {
            if a == b {
                return Err(format!(
                    "child role `{a}` already hosts kind `{}`; `{}` cannot take it",
                    existing.role, kind.role
                ));
            }
        }
    }
    tracing::info!(target: "served_kind", kind = kind.role, "served kind registered");
    kinds.push(kind);
    Ok(())
}

/// Every kind this binary serves, in registration order.
pub fn served_kinds() -> Vec<ServedKind> {
    kinds_in(registry())
}

/// The kinds `registry` holds. A poisoned lock still holds a whole list (the
/// one writer, [`admit`], pushes last), so the list is recovered and the
/// poisoning reported: an empty answer would mount no kind routes and say
/// nothing (ARCH §6).
fn kinds_in(registry: &Registry) -> Vec<ServedKind> {
    match registry.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => {
            tracing::error!(
                target: "served_kind",
                "served kind registry lock is poisoned; serving the kinds it holds"
            );
            poisoned.into_inner().clone()
        }
    }
}

/// The kind a compute child hosts under `role`, if one is registered.
pub fn kind_for_child_role(role: &str) -> Option<ServedKind> {
    served_kinds()
        .into_iter()
        .find(|k| k.child_role() == Some(role))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rerank is a served kind: routed, dialled, and hostable in a child.
    #[test]
    fn rerank_is_registered_with_its_route_client_and_child_role() {
        let kinds = served_kinds();
        let rerank = kinds
            .iter()
            .find(|k| k.role == "rerank")
            .expect("rerank must be a registered kind");
        assert_eq!(rerank.route_path(), Some("/v1/rerank"));
        assert_eq!(
            rerank.client,
            KindClient::Provided {
                method: "rerank_batch"
            }
        );
        assert_eq!(rerank.child_role(), Some("rerank"));
        assert_eq!(
            kind_for_child_role("rerank").map(|k| k.role),
            Some("rerank")
        );
    }

    /// Every named absence names its reason.
    #[test]
    fn every_absence_names_its_reason() {
        for kind in served_kinds() {
            if let KindRoute::Absent { reason } = kind.route {
                assert!(
                    !reason.trim().is_empty(),
                    "{}: route absent with no reason",
                    kind.role
                );
            }
            if let KindClient::Absent { reason } = kind.client {
                assert!(
                    !reason.trim().is_empty(),
                    "{}: client absent with no reason",
                    kind.role
                );
            }
            if let KindChild::Absent { reason } = kind.child {
                assert!(
                    !reason.trim().is_empty(),
                    "{}: child absent with no reason",
                    kind.role
                );
            }
        }
    }

    /// A registry whose lock a writer poisoned still answers its kinds, and
    /// says the lock is poisoned where the daemon's log can see it.
    #[test]
    fn a_poisoned_registry_still_answers_its_kinds_and_says_so() {
        let registry: Registry = RwLock::new(vec![RERANK]);
        let _ = std::thread::scope(|s| {
            s.spawn(|| {
                let _guard = registry.write().unwrap();
                panic!("poison the registry lock");
            })
            .join()
        });
        assert!(registry.is_poisoned());

        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let sink = std::sync::Arc::clone(&logs);
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("served_kind=error")
            .with_writer(move || Sink(std::sync::Arc::clone(&sink)))
            .with_ansi(false)
            .finish();
        let kinds = tracing::subscriber::with_default(subscriber, || kinds_in(&registry));

        assert_eq!(
            kinds.iter().map(|k| k.role).collect::<Vec<_>>(),
            vec!["rerank"],
            "a poisoned lock must not read as no kinds"
        );
        let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
        assert!(
            logged.contains("served kind registry lock is poisoned"),
            "the recovery must be reported: {logged:?}"
        );
    }

    struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A second kind cannot take a name, route or child role already taken.
    #[test]
    fn a_taken_role_route_or_child_role_is_refused() {
        let err = register_kind(RERANK).expect_err("a duplicate role must be refused");
        assert!(err.contains("rerank"), "got: {err}");
        let route_clash = ServedKind {
            role: "other-reranker",
            env_path: None,
            child: KindChild::Absent { reason: "test" },
            ..RERANK
        };
        let err = register_kind(route_clash).expect_err("a taken route must be refused");
        assert!(err.contains("/v1/rerank"), "got: {err}");
    }

    /// No two registered kinds share a role, route or child role: the built-in
    /// seed is admitted through the same check an out-of-tree kind is.
    #[test]
    fn no_two_registered_kinds_clash() {
        let mut admitted = Vec::new();
        for kind in served_kinds() {
            admit(&mut admitted, kind).expect("every registered kind passes the clash check");
        }
    }

    /// The kind's loader carries the residency fit check, so a slot that
    /// cannot fit is refused before anything allocates — in the daemon and
    /// the compute child, not only in the CLI's `load_from_env`.
    #[test]
    fn the_rerank_loader_refuses_a_slot_that_does_not_fit() {
        assert!(
            !crate::capacity::check_skipped_by_env(),
            "SOVEREIGN_SKIP_VRAM_CHECK disables the check this test watches"
        );
        let dir = std::env::temp_dir().join(format!("pb-rerank-fit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("huge-reranker.gguf");
        // Sparse: 4 TiB on paper, no blocks on disk.
        std::fs::File::create(&path)
            .and_then(|f| f.set_len(4 << 40))
            .expect("sparse file");
        let err = match RERANK.load_provider(&path) {
            Ok(_) => panic!("a 4 TiB reranker cannot load"),
            Err(e) => e.to_string(),
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("does not fit"), "got: {err}");
    }

    /// The config key is the kind's role; the env var, when set, wins.
    #[test]
    fn the_models_kinds_key_names_the_path() {
        let kind = ServedKind {
            env_path: None,
            ..RERANK
        };
        let mut models = ModelsSection::default();
        assert_eq!(kind.model_path(Some(&models)), None);
        models
            .kinds
            .insert("rerank".into(), "/m/rerank.gguf".into());
        assert_eq!(
            kind.model_path(Some(&models)),
            Some(PathBuf::from("/m/rerank.gguf"))
        );
    }

    /// The route answers with one score per document, in input order.
    #[tokio::test]
    async fn the_rerank_route_scores_each_document_in_order() {
        struct Scores;
        #[async_trait::async_trait]
        impl InferenceProvider for Scores {
            async fn complete(
                &self,
                _: &sovereign_contracts::types::CompletionRequest,
            ) -> sovereign_contracts::error::Result<sovereign_contracts::types::CompletionResponse>
            {
                unreachable!()
            }
            async fn complete_stream(
                &self,
                _: &sovereign_contracts::types::CompletionRequest,
            ) -> sovereign_contracts::error::Result<
                std::pin::Pin<
                    Box<
                        dyn futures::Stream<Item = sovereign_contracts::error::Result<String>>
                            + Send,
                    >,
                >,
            > {
                unreachable!()
            }
            async fn embed(&self, _: &str) -> sovereign_contracts::error::Result<Vec<f32>> {
                unreachable!()
            }
            async fn rerank_batch(
                &self,
                _: &str,
                docs: &[String],
            ) -> sovereign_contracts::error::Result<Vec<f32>> {
                Ok(docs.iter().map(|d| d.len() as f32).collect())
            }
            fn capabilities(&self) -> sovereign_contracts::types::ProviderCapabilities {
                unreachable!()
            }
        }
        let KindRoute::Served { serve, .. } = RERANK.route else {
            panic!("rerank must be served");
        };
        let body = serde_json::json!({"model": "r", "query": "q", "documents": ["a", "bbb"]});
        let out = serve(Arc::new(Scores), body).await.expect("served");
        let resp: oicp_types::openai_types::RerankResponse = serde_json::from_value(out).unwrap();
        assert_eq!(resp.results.len(), 2);
        assert_eq!(
            (resp.results[1].index, resp.results[1].relevance_score),
            (1, 3.0)
        );
        let empty = serde_json::json!({"query": "q", "documents": []});
        assert!(matches!(
            serve(Arc::new(Scores), empty).await,
            Err(KindServeError::BadRequest(_))
        ));
    }
}
