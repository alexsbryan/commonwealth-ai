// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`RecordedFetch`]: resolver replies recorded once from the live services
//! (`tests/fixtures/records/`), replayed by URL, every URL asked kept, so a
//! test both drives the vendors' readers offline and sees what would have
//! left the host.

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::{FetchReply, RecordFetch, ResolverTable};

/// Recorded replies by URL; a URL with none is unreachable.
#[derive(Debug, Default)]
pub struct RecordedFetch {
    replies: BTreeMap<String, FetchReply>,
    asked: Mutex<Vec<String>>,
}

impl RecordedFetch {
    /// Answer `url` with `status` and `body`.
    pub fn with(mut self, url: String, status: u16, body: &str) -> Self {
        self.replies.insert(
            url,
            FetchReply {
                status,
                body: body.to_string(),
            },
        );
        self
    }

    /// The replies recorded 2026-10-15: Crossref for a DOI it holds and one
    /// it does not, Open Library for an ISBN it holds and one it does not.
    pub fn recorded_2026_10_15() -> Self {
        let t = ResolverTable::from_data_file().expect("the data file loads");
        let row = |v: &str| {
            t.resolvers
                .iter()
                .find(|r| r.variable == v)
                .expect("a resolver for the variable")
                .clone()
        };
        let (doi, isbn) = (row("DOI"), row("ISBN"));
        Self::default()
            .with(
                doi.url_for("10.1038/sdata.2016.18"),
                200,
                include_str!("../../tests/fixtures/records/crossref_10.1038_sdata.2016.18.json"),
            )
            .with(
                doi.url_for("10.0000/oicp-conformance.unresolvable"),
                404,
                include_str!("../../tests/fixtures/records/crossref_unresolvable.txt"),
            )
            .with(
                isbn.url_for("9780262510875"),
                200,
                include_str!("../../tests/fixtures/records/open_library_9780262510875.json"),
            )
            .with(
                isbn.url_for("9780999999990"),
                200,
                include_str!("../../tests/fixtures/records/open_library_9780999999990.json"),
            )
    }

    /// Every URL asked, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("asked lock").clone()
    }
}

#[async_trait::async_trait]
impl RecordFetch for RecordedFetch {
    async fn get(&self, url: &str) -> Result<FetchReply, String> {
        self.asked.lock().expect("asked lock").push(url.to_string());
        self.replies
            .get(url)
            .cloned()
            .ok_or_else(|| format!("no recorded reply for {url}"))
    }
}
