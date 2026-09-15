// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compute's remote isolation: leasing a rented machine and running work on it

pub mod worker_controller;
pub mod worker_http;
pub mod worker_inference_proxy;
pub mod worker_pod;
pub mod worker_subprocess_runner;
