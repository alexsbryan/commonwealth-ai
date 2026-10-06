    /// "A person is waiting" until dropped; `None` when no signal is installed.
    fn foreground_lease(&self) -> Option<corpus_engine_yield::ForegroundLease>;
