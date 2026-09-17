// SPDX-License-Identifier: AGPL-3.0-or-later
//! Answering's non-ATOS middleware pieces.
//!
//! These are the files from `sovereign-api`'s `middleware/` cluster that name
//! neither ATOS nor the host: the tool injector (a concrete [`Middleware`] over
//! `oicp_types::ToolDescriptor`) and the turn-fidelity switches. Domains
//! `REVIEW-build-answering-inversion` split the cluster by what each file
//! names — the ATOS middlewares moved to `sovereign-atos` — and landed these
//! two here, the Answering context's home (`quality/DAEMON_CORE.md` §4.2).
//!
//! [`Middleware`]: crate::middleware::Middleware
pub mod tool_injector;
pub mod turn_fidelity;
