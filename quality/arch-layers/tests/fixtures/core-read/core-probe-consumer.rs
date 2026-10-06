
pub async fn fixed_consumer(source: &dyn CorpusReadPort) {
    let base: &dyn IndexSource = source;
    let _: Result<Vec<IndexInfo>> = base.usable_indexes().await;
    let _: Result<CorpusIndex> = base.open_index(Path::new("fixture-index")).await;

    let _: Result<Vec<f32>> = source.embed("query").await;
    let _: Result<Vec<IndexInfo>> = source.installed_indexes().await;
    let _: Result<CorpusIndex> = source.open_index_for_corpus("fixture-corpus").await;
    let _: &Path = source.index_dir();
    let _: Option<ForegroundLease> = source.foreground_lease();
    let _: Vec<BuiltinCorpus> = source.builtin_corpora();
}

#[cfg(test)]
#[test]
fn core_read_surface_compiles() {
    let _ = fixed_consumer;
}
