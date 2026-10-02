// SPDX-License-Identifier: AGPL-3.0-or-later
//! The table every long-running daemon route keeps its jobs in, at its
//! historical path. It moved to the host kit (`host_kit::jobs`,
//! pb-serve-distributes): a program's binary owns its own job table, and serve's
//! weight downloads keep theirs in the same one.

pub use host_kit::jobs::{JobRegistry, TablePoisoned};
