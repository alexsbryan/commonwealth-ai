// `acquired` is pub(crate): a door outside the defining crate does not exist.
use corpus_index::index::Evidence;
use kernel_types::Custody;

fn main() {
    let _ = Evidence::acquired("the text", Custody::Personal, 0.9);
}
