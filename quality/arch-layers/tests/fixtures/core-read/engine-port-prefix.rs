
/// The whole read surface the svrn turn path calls on the engine: `IndexSource`
/// plus embed, the unfiltered listing, open-by-id, the index root, the
/// foreground lease and the registry catalog. ONE port — list/open stay
/// `IndexSource`'s (five-programs-24 fork (1), fp-64).
#[async_trait]
pub trait CorpusReadPort: IndexSource {
