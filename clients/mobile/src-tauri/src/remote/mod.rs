// SPDX-License-Identifier: AGPL-3.0-or-later
//! Remote transport to the host's `sovereign-server` over the tailnet.

pub mod client;
pub mod dto;
pub mod map;
pub mod stream;

pub use client::ApiClient;
