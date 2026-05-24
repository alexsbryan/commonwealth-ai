//! Convenience wrappers — preset (prompt, polarity,
//! structural-test) combos. Each task is a pure function that
//! consumes a workdir + intent and returns a [`Trial`] the caller
//! hands to [`crate::trial::run_trial`].
//!
//! Tasks are opt-in. Power users build a `Trial` by hand; common
//! cases use a task wrapper for the prompt + test-generator
//! ergonomics.
//!
//! The architectural claim: most "task types" you'd want to
//! support are 30-line files in this directory. Adding a new task
//! shape never requires touching the core loop.

pub mod framework;
pub mod make_passing;
pub mod split_file;
pub mod write_failing_test;

pub use framework::{detect_framework, Framework};
pub use make_passing::make_failing_tests_pass;
pub use split_file::split_file;
pub use write_failing_test::write_failing_test;
