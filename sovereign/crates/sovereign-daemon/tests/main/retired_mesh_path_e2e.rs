// SPDX-License-Identifier: AGPL-3.0-or-later
//! A mesh path retired with no new home answers 410 naming what replaced it,
//! never a bare 404 (pc-bare-404s; FIVE_PROGRAMS §4 rule 3).

use crate::common::mesh_admin_services;

use std::net::SocketAddr;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_daemon::daemon::EmbeddedDaemon;
use sovereign_daemon::mesh_http::{mesh_router, RETIRED};

#[tokio::test]
async fn mesh_measurements_answers_410_naming_the_ring_journal() {
    assert!(
        RETIRED.iter().any(|(p, _)| *p == "/v1/mesh/measurements"),
        "the census lost the path it is about: {RETIRED:?}"
    );
    let tmp = tempfile::tempdir().unwrap();
    let daemon = EmbeddedDaemon::new(
        tmp.path().to_path_buf(),
        SetupConfig::unconfigured(),
        mesh_admin_services(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            mesh_router(daemon).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    let client = reqwest::Client::new();
    for req in [
        client.get(format!(
            "http://{addr}/v1/mesh/measurements?include_self=true"
        )),
        client
            .post(format!("http://{addr}/v1/mesh/measurements"))
            .body("{}"),
    ] {
        let resp = req.send().await.expect("server reachable");
        assert_eq!(resp.status(), reqwest::StatusCode::GONE);
        let body: serde_json::Value = resp.json().await.expect("a JSON body");
        let error = body["error"].as_str().unwrap_or_default();
        assert!(
            error.contains("ring journal") && error.contains("svrn mesh bench"),
            "the 410 must name what replaced the door: {body}"
        );
    }
}
