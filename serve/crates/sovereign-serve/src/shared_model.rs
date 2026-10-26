// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[shared_model] model_id`, published as `SOVEREIGN_SHARED_MODEL_ID` for the
//! one reader that routes on it: `rank`'s construction of the mesh provider
//! (`SharedModelFleet::from_env`). What remains of `distributed_role` after the
//! mesh fabric's deletion (decision engine-swap-8): the rest of that module
//! wrote env the fabric read, and nothing reads it now.

/// Publish the shared model this node routes its primary turns into. An
/// explicitly-set `SOVEREIGN_SHARED_MODEL_ID` wins; an empty id publishes
/// nothing. A process hosting serve's assembly calls this before `rank`.
pub fn apply_shared_model_to_env(cfg: &sovereign_contracts::setup_config::SharedModelSection) {
    let Some(id) = cfg.model_id.as_deref().filter(|id| !id.is_empty()) else {
        tracing::debug!("shared-model: no [shared_model] model_id; primary turns route locally");
        return;
    };
    if std::env::var_os("SOVEREIGN_SHARED_MODEL_ID").is_some() {
        tracing::info!(config_model_id = id, "shared-model: SOVEREIGN_SHARED_MODEL_ID already set; it wins over [shared_model] model_id");
        return;
    }
    std::env::set_var("SOVEREIGN_SHARED_MODEL_ID", id);
    tracing::info!(model_id = id, "shared-model: primary routes to SOVEREIGN_SHARED_MODEL_ID");
}
