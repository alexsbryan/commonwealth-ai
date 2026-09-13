// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mesh surfaces of [`TurnClient`] — the membership itself, and the
//! two mesh-admin reads the Mesh Health panel renders.
//!
//! Split out of `lib.rs` when the mesh mutations joined them (sv-surface
//! svt-3): the file was already over its ceiling, and these methods are
//! one subject rather than a slice of the turn protocol.
//!
//! # Two ports, and it matters which
//!
//! `/v1/mesh/*` is the CLIENT port — `sovereign_mesh::mesh_http`'s router,
//! mounted on the daemon's client listener. `/internal/*` is the INTERNAL
//! port — `sovereign_api::routes_internal::mesh_admin`, loopback-only.
//! A [`TurnClient`] carries one base URL, so a caller builds it from
//! `client_base_url()` for the first family and `internal_base_url()` for
//! the second. They are not interchangeable and a wrong base arrives as a
//! 404 from the host, never as an empty answer.
//!
//! # Why every read is generic
//!
//! `NodeContributionsView` and `PeerPreferenceView` are defined in
//! `commonwealth-api`, and `mesh_http::StatusResponse` in `sovereign-mesh`.
//! This crate's only in-repo dependency is `sovereign-contracts`
//! (`quality/ARCH_LAYERS.toml`, the `contract` layer) — a client that could
//! name those types would be a client that links a daemon, which is the
//! whole thing this crate exists not to do. The shapes a client is OWED
//! live in `sovereign_contracts::daemon_wire::mesh` since svt-3
//! (`MeshStatusSummary`, `KnownMeshDto`, `RelayCandidate`,
//! `JoinConfirmation`, `DaemonIdentity`); each method names the one it
//! expects, and stays generic so a caller with a wider type can still ask.

use super::{Result, TurnClient};

impl TurnClient {
    // ─── The mesh contribution ledger (sv-surface svt-3) ─────

    /// `GET /internal/contribution/view` — every peer's dimensional
    /// contributions over the daemon's default window, one entry per
    /// node its `MeshStore` holds ledger events about.
    ///
    /// The wire form of `commonwealth_state::current_contributions`,
    /// which this crate cannot name (see the note above
    /// [`Self::corpus_atoms`]), so the caller supplies `T`. It is
    /// `Vec<sovereign_api::routes_internal::NodeContributionsView>`
    /// for the daemon's own shape, and the desktop's
    /// `Vec<NodeContributionsDto>` — the same field names — for the
    /// Mesh Health Members panel.
    ///
    /// The host owns the ORDER: it sorts by node id before it answers.
    /// A caller that re-sorts is a second decider for a question
    /// already settled (ARCH principle 8).
    ///
    /// This is the LEDGER, not a liveness probe. An empty list means
    /// the store holds no events — the right answer for a mesh of one
    /// that has served nothing yet, and NOT a signal that the host is
    /// unreachable, which arrives as an `Err`.
    pub async fn contribution_view<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        self.internal_get("/internal/contribution/view".to_string(), &[])
            .await
    }

    // ─── Per-peer affinity preferences (sv-surface svt-3) ────────

    /// `GET /internal/peer-preference/list` — every affinity
    /// preference the host holds, in the store's own scan order.
    ///
    /// `T` is `Vec<sovereign_api::routes_internal::PeerPreferenceView>`
    /// for the daemon's shape and the desktop's `Vec<PeerPreferenceDto>`
    /// — identical field names — for the Mesh Health panel. This crate
    /// cannot name either (see the note above [`Self::corpus_atoms`]).
    ///
    /// An empty list means the operator has set no preferences, which
    /// is the default state of every node. It is NOT "the host could
    /// not be asked" — that arrives as an `Err` (ARCH principle 6).
    pub async fn peer_preferences<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.internal_get("/internal/peer-preference/list".to_string(), &[])
            .await
    }

    /// `POST /internal/peer-preference/set` — set or replace one
    /// peer's multiplier.
    ///
    /// `multiplier` is NOT validated here. The `(0.0, 1.0]` clamp is
    /// `commonwealth_state::PeerPreference::new`'s and a second copy in
    /// this client would be a second decider free to drift from it
    /// (ARCH principle 8); an out-of-range value comes back as the
    /// host's own 400 text.
    pub async fn set_peer_preference(
        &self,
        node_id: &str,
        multiplier: f64,
        reason: Option<&str>,
    ) -> Result<()> {
        let body = serde_json::json!({
            "node_id": node_id,
            "multiplier": multiplier,
            "reason": reason,
        });
        self.internal_write_no_answer(
            reqwest::Method::POST,
            "/internal/peer-preference/set".to_string(),
            Some(&body),
        )
        .await
    }

    /// `POST /internal/peer-preference/clear` — drop one peer's
    /// preference, answering whether one was there.
    ///
    /// The bool is the host's, not an inference from a status code:
    /// "cleared it" and "there was nothing set" are different facts and
    /// the caller renders them differently.
    pub async fn clear_peer_preference(&self, node_id: &str) -> Result<bool> {
        self.internal_post_json(
            "/internal/peer-preference/clear".to_string(),
            &serde_json::json!({ "node_id": node_id }),
        )
        .await
    }

    // ─── The mesh itself: create, join, switch, leave (svt-3) ────
    //
    // These seven ride the CLIENT port (`/v1/mesh/*`), so construct the
    // client with `client_base_url()`, not the internal one. They are
    // `sovereign_mesh::mesh_http`'s routes, which the daemon mounts on
    // its client listener whether it was started by the CLI, by a
    // service manager, or in-process by a Local-mode desktop — so one
    // path answers in every boot mode and the caller does not fork on
    // which (sv-surface svt-3).
    //
    // Every shape is `T: DeserializeOwned` for the same reason the rest
    // of this crate's reads are: `StatusResponse`, `KnownMeshDto` and
    // `RelayCandidate` are defined in `sovereign-mesh`, and this crate's
    // only in-repo dependency is `sovereign-contracts` (ARCH_LAYERS.toml
    // `contract` layer). A client that could name them would be a client
    // that links the daemon.

    /// `GET /v1/mesh/status` — the whole mesh view in one read:
    /// membership, online counts, the invite, every joined mesh, and
    /// this node's own reachability.
    ///
    /// `T` is `sovereign_contracts::daemon_wire::MeshStatusSummary` (the
    /// route's `sovereign_mesh::mesh_http::StatusResponse` for a caller
    /// that links the daemon). It is a READ and it always answers: a node in no mesh reports `running: false`
    /// with `mesh_name: None`, which is a fact and not an absence. An
    /// unreachable host is an `Err` (ARCH principle 6).
    pub async fn mesh_status<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        self.internal_get("/v1/mesh/status".to_string(), &[]).await
    }

    /// `POST /v1/mesh/create` — create a mesh and answer with the
    /// shareable invite.
    ///
    /// The host exposes its client API as part of this (`mesh_http.rs`
    /// `daemon.expose_client_api()`): an explicit create IS the opt-in
    /// to serving remote peers, and that decision is the daemon's, not
    /// a step a caller performs first (ARCH principle 12).
    ///
    /// `name` and `node_name` are `Option` because the route defaults
    /// both — `"<host>'s Mesh"` and the machine hostname — and a second
    /// default spelled here would be a second decider (ARCH principle 8).
    pub async fn mesh_create<T: serde::de::DeserializeOwned>(
        &self,
        name: Option<&str>,
        node_name: Option<&str>,
        encrypt: bool,
    ) -> Result<T> {
        self.internal_post_json(
            "/v1/mesh/create".to_string(),
            &serde_json::json!({
                "name": name,
                "node_name": node_name,
                "encrypt": encrypt,
            }),
        )
        .await
    }

    /// `POST /v1/mesh/join` — join an existing mesh.
    ///
    /// `key_or_url` takes any of the three forms the host's own
    /// `parse_join_argument` accepts: a bare `cwth-…` key, an
    /// `https://…/join/…` URL, or a `sovereign://join/…` deep link. The
    /// discrimination is the host's; a caller that pre-parsed would be
    /// the narrower of two deciders (ARCH principle 8).
    pub async fn mesh_join<T: serde::de::DeserializeOwned>(
        &self,
        key_or_url: &str,
        node_name: Option<&str>,
    ) -> Result<T> {
        self.internal_post_json(
            "/v1/mesh/join".to_string(),
            &serde_json::json!({
                "key_or_url": key_or_url,
                "node_name": node_name,
            }),
        )
        .await
    }

    /// `POST /v1/mesh/join/preview` — what `mesh_join` WOULD join, as a
    /// confirmation-dialog payload, without joining.
    ///
    /// `T` is `sovereign_contracts::daemon_wire::JoinConfirmation`. The
    /// HOST parses the invite, with the same three-form parser the join
    /// uses, so a preview cannot refuse what the join would accept. An
    /// invite the host cannot read — or a guest link, which is not a
    /// join — is an `Err` carrying the host's words (400), never an
    /// empty confirmation (ARCH principle 6).
    pub async fn mesh_join_preview<T: serde::de::DeserializeOwned>(&self, link: &str) -> Result<T> {
        self.internal_post_json(
            "/v1/mesh/join/preview".to_string(),
            &serde_json::json!({ "link": link }),
        )
        .await
    }

    /// `GET /status` — the serving host's own identity and health, on the
    /// CLIENT port (`sovereign_api::routes_status`; auth-exempt).
    ///
    /// `T` is `sovereign_contracts::daemon_wire::DaemonIdentity` for a
    /// caller that wants the host's `node_id` — the desktop's corpus engine
    /// partitions by it, and until svt-3 the app read the daemon's
    /// `<data_dir>/node_id` FILE to learn it, generating one when the file
    /// was absent (a second minter of the daemon's identity). A client asks
    /// the daemon; an unreachable host is an `Err`, never a fresh id.
    pub async fn daemon_status<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        self.internal_get("/status".to_string(), &[]).await
    }

    /// `POST /v1/mesh/rotate` — mint a fresh join key for the active
    /// mesh. Existing members stay connected; only future joins need
    /// the new link.
    ///
    /// `force` is the route's own `?force=` query. The handler takes no
    /// body, so this sends none.
    pub async fn mesh_rotate<T: serde::de::DeserializeOwned>(&self, force: bool) -> Result<T> {
        let path = if force {
            "/v1/mesh/rotate?force=true".to_string()
        } else {
            "/v1/mesh/rotate".to_string()
        };
        self.internal_post_bare(path).await
    }

    /// `POST /v1/mesh/switch` — park the active mesh and bring another
    /// joined one up.
    ///
    /// The host rebinds its client listener as part of this, so the
    /// caller must poll back through the bounce. It resolves `mesh`
    /// against its own membership BEFORE detaching, so an unknown name
    /// arrives as a readable 404 rather than an accepted request
    /// followed by silence.
    pub async fn mesh_switch(&self, mesh: &str) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::POST,
            "/v1/mesh/switch".to_string(),
            Some(&serde_json::json!({ "mesh": mesh })),
        )
        .await
    }

    /// `POST /v1/mesh/forget` — drop a PARKED mesh from this node.
    ///
    /// `mesh` takes a name, a full hex id or an unambiguous id prefix;
    /// the host resolves it through the same `resolve_known` that Switch
    /// uses. Forgetting the ACTIVE mesh is refused in the host's own
    /// words.
    pub async fn mesh_forget(&self, mesh: &str) -> Result<()> {
        self.internal_write_no_answer(
            reqwest::Method::POST,
            "/v1/mesh/forget".to_string(),
            Some(&serde_json::json!({ "mesh": mesh })),
        )
        .await
    }

    /// `POST /v1/mesh/leave` — leave the current mesh and return this
    /// node to a fresh solo mesh.
    ///
    /// The host ACKs and re-solos in a detached task: the handler is
    /// being served by the very listener that leaving drops, so tearing
    /// down inline would cancel its own response mid-flight. The
    /// listener is back within about a second, and the caller polls for
    /// it — that wait is the route's shape, not a client-side guess.
    pub async fn mesh_leave(&self) -> Result<()> {
        self.internal_write_no_answer::<()>(
            reqwest::Method::POST,
            "/v1/mesh/leave".to_string(),
            None,
        )
        .await
    }

    /// `GET /v1/mesh/relay-candidates` — the reachable addresses
    /// (Tailscale / LAN / IPv6) a user can append to an invite as
    /// `?relay=<host:port>` for a friend mDNS will not reach.
    ///
    /// `T` is `sovereign_contracts::daemon_wire::RelayCandidate`. The
    /// HOST answers this because the addresses are its own: it may be
    /// in a container or bound differently from whatever asked. An
    /// empty list means no detected interface, which is a fact — the
    /// caller hides the picker rather than reporting an error.
    pub async fn mesh_relay_candidates<T: serde::de::DeserializeOwned>(&self) -> Result<Vec<T>> {
        #[derive(serde::Deserialize)]
        struct Body<T> {
            candidates: Vec<T>,
        }
        let body: Body<T> = self
            .internal_get("/v1/mesh/relay-candidates".to_string(), &[])
            .await?;
        Ok(body.candidates)
    }
}
