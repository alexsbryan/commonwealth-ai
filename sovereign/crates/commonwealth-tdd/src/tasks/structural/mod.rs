//! Structural goal templates — language-agnostic.
//!
//! Each module here renders a per-framework structural test file
//! that materializes a goal as an executable pytest / cargo-test /
//! vitest / jest / go-test assertion. The `MaximizePassing`
//! polarity then drives toward that test passing.
//!
//! New structural goals (function-count-per-file, max-function-
//! length, etc.) land as new files in this directory. Each goal's
//! file has a `render(framework, params) -> (PathBuf, String)`
//! function that picks the right template — single source of truth
//! per goal × language cell.

pub mod max_file_size;

pub use max_file_size::render as render_max_file_size;
