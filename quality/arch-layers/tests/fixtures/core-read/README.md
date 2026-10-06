# Core read-port compiler projection

This four-package workspace checks whether a fixed svrn caller can compile against the historical two-method `IndexSource` base plus a selected subset of the six `CorpusReadPort` declarations. `corpus-index` may depend on the `corpus-engine-yield` leaf only. `core-probe` may reach the shared leaves but is forbidden from reaching `engine-probe`.

`target = "contract"` places the extension in `corpus-index::source`; `target = "engine"` places the same selected declaration set in `engine-probe`. `binding = "port"` imports from the contract package; `binding = "engine"` imports from the engine package. The caller and test are fixed templates, not candidate-authored source.

The `SOURCE_CONTRACT.toml` file binds this projection to historical commits and blobs and lists every nominal substitution. In particular, the test establishes compiler reachability and declared package edges only. It does not exercise or claim corpus-engine, LanceDB, cache, embedding, filesystem, or foreground-lease behavior.
