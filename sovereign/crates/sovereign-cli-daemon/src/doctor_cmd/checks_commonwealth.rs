// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Commonwealth layer of `svrn doctor` — the mesh daemon, this node's
//! membership in it, and whether the mesh can actually serve inference.
//! Every check here talks to the daemon over HTTP and treats an unreachable
//! daemon as a reported absence, never as a pass.

use super::probe::{http_get_json, http_post_json, tcp_connectable};
use super::{CheckResult, CheckStatus, Layer, Repair};

/// Is cw-rails started at boot? After the flip it is the node's mesh endpoint,
/// and nothing but its own unit restarts it, so a node whose unit is disabled
/// leaves the mesh at the next reboot (phase-b-51). `svrn mesh up` installs the
/// unit; a node that never ran it has none, which is reported, not failed.
pub(super) fn check_rails_boot_unit() -> CheckResult {
    let state = if cfg!(target_os = "linux") {
        Some(host_kit::service::SystemdUser::system().state(sovereign_turn_client::rails_kv::RAILS_UNIT))
    } else {
        None
    };
    tracing::debug!(state = ?state, "doctor: cw-rails boot unit");
    rails_boot_unit_result(state)
}

/// The check's verdict for one `is-enabled` reading (`None`: no systemd user
/// units on this platform).
pub(super) fn rails_boot_unit_result(state: Option<host_kit::service::UnitState>) -> CheckResult {
    use host_kit::service::UnitState;
    let unit = sovereign_turn_client::rails_kv::RAILS_UNIT;
    let up = sovereign_turn_client::rails_kv::RAILS_BRING_UP_VERB;
    let (status, message, repair) = match state {
        Some(UnitState::Enabled) => (
            CheckStatus::Passed,
            format!("{unit} is enabled: cw-rails starts at boot"),
            Repair::None,
        ),
        Some(UnitState::Disabled(how)) => (
            CheckStatus::Warning,
            format!(
                "{unit} is {how}: cw-rails will NOT start at boot, so this node is off \
                 the mesh after a reboot until `{up}` runs"
            ),
            Repair::executable(up),
        ),
        Some(UnitState::NotInstalled) => (
            CheckStatus::Skipped,
            format!("no {unit}: this node has not run `{up}`, which installs it"),
            Repair::None,
        ),
        Some(UnitState::Unknown(why)) => (
            CheckStatus::Warning,
            format!("could not read {unit}'s state: {why}"),
            Repair::None,
        ),
        None => (
            CheckStatus::Skipped,
            format!(
                "no {unit} on {}: only systemd user units are written; cw-rails starts \
                 at boot only under a service manager you set up",
                std::env::consts::OS
            ),
            Repair::None,
        ),
    };
    CheckResult {
        name: "rails_boot_unit",
        layer: Layer::Commonwealth,
        status,
        message,
        repair,
    }
}

pub(super) async fn check_daemon_running() -> CheckResult {
    let up = tcp_connectable("127.0.0.1", 9741).await;
    if up {
        CheckResult {
            name: "daemon_running",
            layer: Layer::Commonwealth,
            status: CheckStatus::Passed,
            message: "commonwealth daemon reachable at :9741".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "daemon_running",
            layer: Layer::Commonwealth,
            status: CheckStatus::Failed,
            message: "commonwealth daemon not reachable at :9741".into(),
            repair: Repair::executable("svrn daemon start"),
        }
    }
}

pub(super) async fn check_mesh_member(client_url: &str) -> CheckResult {
    // The real status endpoint lives on the client listener
    // (`:9741/status`), not the internal port. Shape is
    // `{node_id, mesh: {name, members_online, members_total, ...}, ...}`.
    let url = format!("{client_url}/status");
    match http_get_json(&url).await {
        Some(json) => {
            let total = json["mesh"]["members_total"].as_u64().unwrap_or(0);
            let online = json["mesh"]["members_online"].as_u64().unwrap_or(0);
            let name = json["mesh"]["name"].as_str().unwrap_or("<unknown>");
            if total > 1 {
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!("member of \"{name}\" — {online}/{total} online"),
                    repair: Repair::None,
                }
            } else if total == 1 {
                // Solo mesh is the default on a freshly-setup single
                // machine. Not an error — just informational.
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!(
                        "solo mesh \"{name}\" — run `svrn mesh create` to invite peers"
                    ),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Warning,
                    message: "daemon running but no mesh formed yet".into(),
                    repair: Repair::Manual("Run `svrn mesh create` or accept a join link".into()),
                }
            }
        }
        None => CheckResult {
            name: "mesh_member",
            layer: Layer::Commonwealth,
            status: CheckStatus::Failed,
            message: format!("could not reach {url}"),
            repair: Repair::executable("svrn daemon restart"),
        },
    }
}

/// H3 egress posture: report whether mesh traffic is on iroh and via
/// which path, plus whether an HTTP(S) proxy is engaged for the relay
/// (credentials redacted). Informational — a mesh on the IP path is
/// perfectly valid; this exists so a netops operator can confirm from
/// one command what the node touches.
pub(super) async fn check_iroh_egress(client_url: &str) -> CheckResult {
    // Local proxy posture, mirroring iroh's HTTPS_PROXY→HTTP_PROXY
    // precedence, userinfo redacted.
    let proxy = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.trim().is_empty()))
        .map(|u| match (u.find("://"), u.find('@')) {
            (Some(s), Some(at)) if at > s + 3 => format!("{}***@{}", &u[..s + 3], &u[at + 1..]),
            _ => u,
        });
    let proxy_note = match &proxy {
        Some(p) => format!("; proxy={p} (Basic auth only — NTLM/Kerberos unsupported)"),
        None => String::new(),
    };

    let url = format!("{client_url}/v1/mesh/status");
    match http_get_json(&url).await {
        Some(json) => {
            let paths: Vec<&str> = json["iroh_transport"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p["path"]["path"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            let msg = if paths.is_empty() {
                format!("mesh on the IP path (iroh not carrying peer traffic){proxy_note}")
            } else {
                format!(
                    "mesh carrying traffic over iroh — peer paths: {}{proxy_note}",
                    paths.join(", ")
                )
            };
            CheckResult {
                name: "iroh_egress",
                layer: Layer::Commonwealth,
                status: CheckStatus::Passed,
                message: msg,
                repair: Repair::None,
            }
        }
        None => CheckResult {
            name: "iroh_egress",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: format!("could not read mesh status from {url}{proxy_note}"),
            repair: Repair::None,
        },
    }
}

pub(super) async fn check_inference_capable(client_url: &str) -> CheckResult {
    // The daemon exposes `inference.loaded_models` on `/status`.
    // Earlier versions had a flat `inference_capable` bool at the top
    // level; that field no longer exists — infer capability from the
    // loaded-models array instead. Empty = cold-start (no model yet
    // loaded), but `/v1/models` still lists available slots.
    let url = format!("{client_url}/status");
    match http_get_json(&url).await {
        Some(json) => {
            let loaded = json["inference"]["loaded_models"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            let models_url = format!("{client_url}/v1/models");
            let registered = http_get_json(&models_url)
                .await
                .and_then(|j| j["data"].as_array().map(|a| a.len()))
                .unwrap_or(0);
            let capable = loaded > 0 || registered > 0;
            if capable {
                CheckResult {
                    name: "inference_capable",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!(
                        "{registered} model(s) registered, {loaded} currently resident"
                    ),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "inference_capable",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Warning,
                    message: "no models registered — /v1/models is empty. restart the daemon after `svrn setup` completes.".into(),
                    repair: Repair::executable("svrn daemon restart"),
                }
            }
        }
        None => CheckResult {
            name: "inference_capable",
            layer: Layer::Commonwealth,
            status: CheckStatus::Skipped,
            message: "commonwealth daemon unreachable — skipping".into(),
            repair: Repair::None,
        },
    }
}

pub(super) async fn check_activity_reporting(internal_url: &str) -> CheckResult {
    // The endpoint expects `{level: "hot"|"warm"|"cool"|..., reason: "..."}`
    // and replies 204. Passing `activity_level: 0.0` (the previous
    // payload) yielded a 422 — it's a string enum, not a float.
    let url = format!("{internal_url}/internal/node/activity");
    let resp = http_post_json(
        &url,
        serde_json::json!({
            "level": "cool",
            "reason": "doctor health check"
        }),
    )
    .await;
    match resp {
        Some(r) if r.status().as_u16() == 204 || r.status().is_success() => CheckResult {
            name: "activity_reporting",
            layer: Layer::Commonwealth,
            status: CheckStatus::Passed,
            message: "activity reporting endpoint reachable".into(),
            repair: Repair::None,
        },
        Some(r) => CheckResult {
            name: "activity_reporting",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: format!("activity endpoint returned HTTP {}", r.status()),
            repair: Repair::Manual("Add commonwealth url to sovereign server config".into()),
        },
        None => CheckResult {
            name: "activity_reporting",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: "could not reach activity reporting endpoint".into(),
            repair: Repair::Manual("Add commonwealth url to sovereign server config".into()),
        },
    }
}

// OmO checks
