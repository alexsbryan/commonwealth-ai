// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-witness pipeline — copy held-out fixtures into the workdir,
//! run `verify_cmd`, parse the test-runner output, score a pass
//! fraction. Per-language parsers live in `test_result_parser.rs`.

pub mod auto_test;
pub mod test_result_parser;

pub use auto_test::{run_auto_witness, AutoWitnessError, AutoWitnessOutcome};
pub use commonwealth_tdd::TestParseResult;
pub use test_result_parser::parse_test_output;
