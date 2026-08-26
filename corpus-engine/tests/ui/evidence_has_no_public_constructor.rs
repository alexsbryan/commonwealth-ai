// There is no `new`, and `acquired` is pub(crate). A door outside the crate
// does not exist under any spelling.
use corpus_engine::{Custody, Evidence};

fn main() {
    let _ = Evidence::new("the text", Custody::Personal, 0.9);
    let _ = Evidence::acquired("the text", Custody::Personal, 0.9);
}
