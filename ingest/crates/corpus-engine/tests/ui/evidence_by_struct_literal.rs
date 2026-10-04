// Every field supplied, and still refused: the fields are private, so a
// struct literal is not a door either.
use corpus_engine::{
    ContentHash, CorpusId, Custody, Evidence, Grain, Locator, Origin, Server, Source,
};

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
        custody: Custody::Personal,
        score: 0.9,
    };
}
