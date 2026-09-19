// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compute's remote isolation: leasing a rented machine and running work on it

pub mod multi_pod_coordinator;
pub mod worker_controller;
pub mod worker_daemon;
pub mod worker_http;
pub mod worker_inference_proxy;
pub use sovereign_contracts::worker_pod; // shim: moved by domains REVIEW-build-serving-worker-port
pub mod worker_subprocess_runner;
