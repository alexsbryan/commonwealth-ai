// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus registry's reconcile moved to `sovereign_core::corpus_registry`,
//! where the turn path can reach it; re-exported at its historical path.

pub(super) use sovereign_core::corpus_registry::reconcile_corpus_registry;
