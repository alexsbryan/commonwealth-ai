//! Generic ingestion-pipeline driver.
//!
//! See `recipe.rs` for the per-corpus surface and `driver.rs` for the
//! claim/dispatch/ack loop. The two are coupled only through the
//! `Worklist` primitive in `worklist.rs`, so swapping out the driver
//! (or running a non-driver consumer like the dashboard) is trivial.

pub mod classifier;
pub mod driver;
pub mod recipe;
pub mod status;
pub mod worklist;

pub use driver::{run_recipe, DriverConfig, RunSummary, Shutdown};
pub use recipe::{Recipe, RecipeError};
pub use status::{report, StatusReport};
pub use worklist::{State, Stats, WorkUnit, Worklist, WorklistError};
