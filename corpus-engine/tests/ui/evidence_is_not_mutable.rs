// `ScoredChunk` is a mutable accumulator — sovereign reassigns `score` at
// twelve production sites and calls `metadata.insert` after construction at
// sixteen. Evidence that has been vouched for does not get edited afterwards.
fn rescore(ev: &mut corpus_engine::Evidence) {
    ev.score = 1.0;
}

fn main() {}
