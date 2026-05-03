//! `AtlasIngestion` implementations.
//!
//! Each module here defines one strategy and a `register_into` hook
//! that the registry calls. Strategies stay self-contained — registry
//! itself imports nothing strategy-specific beyond the trait.

pub mod structure_first;
