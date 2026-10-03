// A Citation not pointing into a sealed EvidenceSet — nc-thesis declared
// illegal construction #4, proven at rung nc-11-answer.
//
// The quote below does not appear in the origin it claims, and nothing here
// could tell: there is no seal in the expression. `Citation::pointing_into`
// is the only door and it takes one, so a citation that exists is a citation
// some seal vouched for.
//
// The `Citation::new` probe lives in its own fixture rather than here,
// because rustc SUPPRESSES this file's private-field diagnostic when method
// resolution in the same body has already failed — observed 2026-08-20 while
// writing this suite. Two illegal writes in one fixture meant one recorded
// error, and a future regression on the struct literal would have been
// invisible behind the other.
use kernel_types::{Citation, ContentHash, CorpusId, Custody, Grain, Locator, Origin, Server, Source};

fn origin() -> Origin {
    Origin {
        source: Source::Corpus {
            corpus: CorpusId::new("wikipedia").unwrap(),
            document: ContentHash::of(b"the whale is a mammal"),
            locator: Locator::new("chunk:42").unwrap(),
        },
        served_by: Server::Local,
        grain: Grain::Leaf,
    }
}

fn main() {
    let _ = Citation {
        quote: "the whale is a fish".to_string(),
        source: origin(),
        custody: Custody::PublicWeb,
    };

}
