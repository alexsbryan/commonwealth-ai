// SPDX-License-Identifier: AGPL-3.0-or-later
//! The served model kinds' routes, by the paths their wire is named at here:
//! what svrn forwards to serve's kind mount (pb-serve-distributes), since svrn
//! links no kind registry. The registry stays the loader's
//! (`sovereign_inference::served_kind`); sovereign-compute's census test holds
//! the registered routes and this list equal, so a kind that mounts a route
//! names its path here or goes red.

/// Every served kind's route, in the order the kinds register.
pub const SERVED_KIND_PATHS: &[&str] = &[crate::rerank_kind::RERANK_PATH, crate::ner::NER_PATH];
