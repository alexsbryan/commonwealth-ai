//! `commonwealth-app` — mesh application platform layer.
//!
//! Provides:
//! - `MeshAppManifest` — static app description, gossiped across nodes.
//! - `AppRegistry` — in-memory registry of known apps.
//! - `AppProcess` — lifecycle management for locally running apps.
//! - `AppPortMap` + `forward` — HTTP reverse-proxy helpers.

pub mod lifecycle;
pub mod manifest;
pub mod proxy;
pub mod registry;

pub use lifecycle::{AppProcess, AppStatus};
pub use manifest::{AppPermissions, MeshAppManifest, RequiredCapabilities};
pub use proxy::{AppPortMap, forward, proxy_client};
pub use registry::AppRegistry;
