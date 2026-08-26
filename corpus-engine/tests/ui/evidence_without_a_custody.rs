// An Evidence with no Custody. `nc-thesis` illegal construction #2.
use corpus_engine::{ContentHash, CorpusId, Evidence, Grain, Locator, Origin, Server, Source};

fn main() {
    let _ = Evidence {
        content: "the text".to_string(),
        origin: Origin {
            source: Source::Corpus {
                corpus: CorpusId::new("wikipedia").unwrap(),
                document: ContentHash::of_str("doc"),
                locator: Locator::new("chunk:1").unwrap(),
            },
            served_by: Server::Local,
            grain: Grain::Leaf,
        },
        score: 0.9,
    };
}
