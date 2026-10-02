// SPDX-License-Identifier: AGPL-3.0-or-later
//! The setup reads' routes, driven over a real listener in the shape serve
//! binds them (moved from the svrn daemon's assets_http.rs by
//! pb-serve-distributes, where they drove the daemon's in-process answers of
//! the same functions).

use super::*;
use sovereign_contracts::daemon_wire::{ProfileName, SlotConfig};

/// [`bundle`] served on a free loopback port. Returns the base URL.
async fn spawn() -> String {
    let app = host_kit::shell::mount(vec![bundle()]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    format!("http://{addr}")
}

/// The three plan reads answer the CONTRACTS types, so a client parses one
/// vocabulary whether it asked the daemon or spawned `svrn setup --plan`.
///
/// Watched fail: point `setup_catalog` at a different profile than the one
/// `resolve_profile` returned and the `profile=cpu_only` assertion goes
/// red (a cpu_only catalog cannot carry a very_high row).
#[tokio::test]
async fn the_plan_reads_answer_the_contracts_shapes() {
    let base = spawn().await;
    let c = reqwest::Client::new();

    let hw: HardwareView = c
        .get(format!("{base}/v1/admin/hardware"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("hardware parses as HardwareView");
    assert!(hw.hardware.system_ram_bytes > 0, "the probe ran");
    assert!(ProfileName::ALL.contains(&hw.profile));

    let catalog: Vec<sovereign_contracts::daemon_wire::PrimaryOption> = c
        .get(format!("{base}/v1/admin/setup/catalog?profile=cpu_only"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("catalog parses as Vec<PrimaryOption>");
    assert!(!catalog.is_empty());
    assert!(
        catalog.iter().all(|o| o.profile == "cpu_only"),
        "a cpu_only catalog carries only cpu_only rows: {:?}",
        catalog.iter().map(|o| &o.profile).collect::<Vec<_>>()
    );

    let slot: Option<SlotConfig> = c
        .get(format!(
            "{base}/v1/admin/setup/slot?kind=embed&profile=default"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("slot parses as Option<SlotConfig>");
    assert!(slot
        .expect("default defines an embed slot")
        .file
        .ends_with(".gguf"));
}

/// An unrecognised profile or slot kind is a 400 naming what exists — not
/// a quiet fall back to `default`, which would hand a client a catalog for
/// a tier it did not ask about.
///
/// Watched fail: replace `ProfileName::from_wire(s).ok_or_else(..)` with
/// `.unwrap_or(ProfileName::Default)` and both halves go green on 200.
#[tokio::test]
async fn an_unknown_profile_or_kind_is_refused_by_name() {
    let base = spawn().await;
    let c = reqwest::Client::new();

    let r = c
        .get(format!("{base}/v1/admin/setup/catalog?profile=gigantic"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(
        r.text().await.unwrap().contains("very_high"),
        "names the set"
    );

    let r = c
        .get(format!("{base}/v1/admin/setup/slot?kind=thoughtful"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::BAD_REQUEST);
}
