// There is no `new`. `acquired` is pub(crate), proven where it is defined:
// corpus-index tests/evidence_reds.rs.
use corpus_engine::{Custody, Evidence};

fn main() {
    let _ = Evidence::new("the text", Custody::Personal, 0.9);
}
