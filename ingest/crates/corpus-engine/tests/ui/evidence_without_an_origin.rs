// An Evidence with no Origin. `nc-thesis` illegal construction #1.
use corpus_engine::{Custody, Evidence};

fn main() {
    let _ = Evidence {
        content: "the text".to_string(),
        custody: Custody::Personal,
        score: 0.9,
    };
}
