// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where people reach the project: one place for the URLs every surface
//! prints, so the desktop's crash report and `svrn doctor` cannot point at
//! different repositories when it moves.

/// The public repository's new-issue form. A surface that has just failed
/// prints it beside what to attach; nothing is uploaded on the user's behalf.
pub const ISSUES_URL: &str = "https://github.com/alexsbryan/commonwealth-ai/issues/new";
