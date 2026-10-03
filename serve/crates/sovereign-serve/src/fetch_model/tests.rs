// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

const ME: &str = "0000000000000000000000000000000a";
const PEER: &str = "0000000000000000000000000000000b";

/// Serve's own model-files origin (`model_transfer::bundle`) over `files`,
/// with the connect info its loopback guard reads.
async fn model_origin(files: Vec<PathBuf>) -> String {
    let servable = sovereign_serving_host::state::ServableModelFilesReader::default();
    servable.publish(files);
    let app = host_kit::shell::mount(vec![sovereign_compute::model_transfer::bundle(servable)]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    base
}

/// cw-rails' roster (this node and one peer) and its reach door, which hands
/// `origin` for the peer on `model_transfer` and nothing on any other class.
async fn stub_rails(origin: String) -> String {
    use axum::extract::Query;
    let member = |name: &str, hex: &str, is_self: bool| {
        serde_json::json!({
            "name": name, "node_id": "node-0000000000000000", "node_id_hex": hex,
            "status": "online", "last_seen": 1, "is_self": is_self,
            "capabilities": {
                "hardware": {"gpus": [], "system_ram_gb": 0, "cpu_cores": 0,
                             "total_storage_gb": 0, "free_storage_gb": 0},
                "available": {"free_vram_gb": 0.0, "free_ram_gb": 0.0, "free_storage_gb": 0.0,
                              "gpu_utilization": 0.0, "cpu_utilization": 0.0,
                              "available_for_mesh": false},
                "hosted_corpora": [], "reported_at": 0, "anchor": null
            },
            "dial": {"relay_url": null, "iroh_direct_addrs": ["192.168.1.20:41000"]}
        })
    };
    let status = serde_json::json!({
        "self": {"node_id": "node-0000000000000000", "node_id_hex": ME, "name": "me"},
        "mesh": {"id": "m", "name": "fetch-mesh"},
        "members": [member("me", ME, true), member("holder", PEER, false)]
    });
    let reach = move |Query(q): Query<mesh_reach::door::ReachQuery>| {
        let origin = origin.clone();
        async move {
            assert_eq!(q.peer, PEER, "the door is never asked for this node");
            let endpoints = if q.class == TrafficClass::ModelTransfer.as_str() {
                vec![mesh_reach::PeerEndpoint {
                    base_url: origin,
                    label: "stub:model_transfer".into(),
                }]
            } else {
                Vec::new()
            };
            axum::Json(mesh_reach::door::Reach {
                peer: "holder".into(),
                node_id: q.peer,
                class: q.class,
                endpoints,
            })
        }
    };
    let app = axum::Router::new()
        .route(
            "/v1/mesh/status",
            axum::routing::get(move || {
                let status = status.clone();
                async move { axum::Json(status) }
            }),
        )
        .route(mesh_reach::door::REACH_PATH, axum::routing::get(reach));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

/// A peer on cw-rails' roster that holds the model is found through the
/// reach door on `model_transfer`, never this node, and the file comes over
/// serve's registered model-files origin, hash-checked. Failing inputs: a
/// discovery that reads a file (mesh.json) or dials a member's `addresses`,
/// a base for this node, or a class other than `model_transfer`.
#[tokio::test]
async fn a_peer_on_cw_rails_roster_serves_the_model_over_serves_origin() {
    let holder = tempfile::tempdir().unwrap();
    let gguf = holder.path().join("m.gguf");
    std::fs::write(&gguf, b"GGUF fetched through cw-rails").unwrap();
    let origin = model_origin(vec![gguf]).await;
    let rails = stub_rails(origin.clone()).await;

    let bases = peer_model_bases(&rails).await.expect("cw-rails' roster");
    assert_eq!(bases, vec![origin]);

    let http = reqwest::Client::new();
    let listing = sovereign_serving_host::model_fetch::list_peer_files(&http, &bases[0], None)
        .await
        .expect("the origin's listing");
    let info = listing
        .files
        .into_iter()
        .find(|f| f.name == "m.gguf")
        .expect("the holder advertises m.gguf");
    let dest = tempfile::tempdir().unwrap();
    let saved = sovereign_serving_host::model_fetch::fetch_model_to_dir(
        &http,
        &bases[0],
        &info,
        dest.path(),
        None,
        |_, _| {},
    )
    .await
    .expect("the fetch");
    assert_eq!(
        std::fs::read(saved).unwrap(),
        b"GGUF fetched through cw-rails"
    );
}

/// No cw-rails answering is refused by name, never read as "no peer could
/// serve" and never a missing-file I/O error.
#[tokio::test]
async fn no_cw_rails_is_refused_by_name() {
    let why = peer_model_bases("http://127.0.0.1:9")
        .await
        .expect_err("no roster");
    assert!(why.contains("cw-rails absent"), "{why}");
}
