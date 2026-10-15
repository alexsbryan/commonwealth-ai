// SPDX-License-Identifier: AGPL-3.0-or-later
//! The v0.6 checks that install a shelf of their own, green against the
//! lawful fake and red against the fake that breaks exactly their field
//! (oicp-v0.6.md §6).

use crate::evidence_checks::v05_feature_failures;
use crate::fake_host::Breaks;
use crate::located_checks::check_knowledge_locator;
use crate::v05_tests::{bench, green, red};

/// v0.6 §2.13's two failing inputs: a span given the heading path of the
/// part before the one it stands in, and a locator invented for a span
/// that stands under no heading (a preface, a text with none).
#[tokio::test]
async fn knowledge_locator_is_red_against_the_part_before_and_an_invented_path() {
    let b = bench(Breaks::default()).await;
    green(&check_knowledge_locator(&b.host, &b.manifest).await);
    assert!(v05_feature_failures(&b.manifest).is_empty());
    for (breaks, says) in [
        (
            Breaks {
                locator_previous_part: true,
                ..Breaks::default()
            },
            "carries under [\"BOOK I. OF THE HARBOUR.\"]; it stands under [\"BOOK I. OF THE \
             HARBOUR.\", \"CHAPTER I. OF THE TIDES.\"]",
        ),
        (
            Breaks {
                locator_invented: true,
                ..Breaks::default()
            },
            "carries under [\"Annals of a Small Port\"]; it stands no locator",
        ),
    ] {
        let b = bench(breaks).await;
        red(&check_knowledge_locator(&b.host, &b.manifest).await, says);
    }
    let mut m = b.manifest.clone();
    m.features.retain(|f| f != "knowledge:range");
    let f = v05_feature_failures(&m);
    assert!(
        f.iter()
            .any(|x| x == "knowledge:locator requires knowledge:range"),
        "{f:?}"
    );
}
