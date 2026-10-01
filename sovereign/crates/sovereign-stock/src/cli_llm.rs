// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-cli-llm-stock` — svrn's LLM verbs with ingest composed in, the
//! binary the dispatcher execs for them (pb-cli-llm-ingest-move-compose;
//! FIVE_PROGRAMS §2c). cli-llm links no engine assembly: this distribution
//! hands its entry the same ingest composition the stock daemon gets, and
//! the bare `sovereign-cli-llm` runs without one.

mod ingest;

fn main() {
    sovereign_cli_llm::bin_main_with(Some(ingest::hosted(Some(Box::new(
        sovereign_recipe_author::port::compose,
    )))))
}
