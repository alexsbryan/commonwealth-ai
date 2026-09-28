// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest ingest`'s refusals, asserted on the sentence a person reads.
//! Moved from corpus-mcp's tests/verbs.rs with the verb (pb-ingest-cli).
//! Nothing here needs a live endpoint.

use std::path::Path;
use std::process::Command;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_svrn-ingest"))
        .current_dir(dir)
        .args(args)
        // The ladder's daemon rung points at a dead port, so a developer's
        // running daemon cannot change the result.
        .env("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:1")
        .output()
        .expect("svrn-ingest did not start");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ── `ingest` — the endpoint combinations, none of which need an endpoint ────

/// §18.3: a half-named pair is refused, never guessed at. Guessing would send
/// phase 1's chat calls to the embedding process.
#[test]
fn ingest_refuses_a_half_specified_endpoint_pair() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe = tmp.path().join("r.toml");
    std::fs::write(&recipe, "# never read: the refusal precedes any disk\n").unwrap();
    let rp = recipe.to_str().unwrap();

    let r = run(tmp.path(), &["ingest", rp, "--chat-url", "http://x/v1"]);
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--embed-url"),
        "the refusal does not name the missing flag:\n{}",
        r.stderr
    );

    let r = run(tmp.path(), &["ingest", rp, "--embed-url", "http://x/v1"]);
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--chat-url"),
        "the refusal does not name the missing flag:\n{}",
        r.stderr
    );

    let r = run(
        tmp.path(),
        &[
            "ingest",
            rp,
            "--base-url",
            "http://x/v1",
            "--chat-url",
            "http://y/v1",
        ],
    );
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--base-url"),
        "mixing --base-url with a half-pair was not refused by name:\n{}",
        r.stderr
    );
}

/// `corpus ingest` with no flags at all is the literal §4 command line, so it
/// walks the same ladder — one implementation of "which endpoint", shared with
/// `serve` (ARCH §10.6). Same assertion, other verb: if `ingest` ever grew its
/// own ladder, one of these two would drift.
#[test]
fn ingest_with_no_endpoint_walks_the_same_ladder() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe = tmp.path().join("r.toml");
    std::fs::write(&recipe, "# never read\n").unwrap();
    let r = run(tmp.path(), &["ingest", recipe.to_str().unwrap()]);
    assert_ne!(r.code, 0);
    for rung in ["ollama", "llama-server", "oicp daemon"] {
        assert!(
            r.stderr.contains(rung),
            "`ingest` did not walk the `{rung}` rung:\n{}",
            r.stderr
        );
    }
}

// ── `pull` — serve's pull-if-absent, moved here with the engine it needs ────

/// `corpus-mcp serve --corpus <absent>` execs this verb (pb-corpus-mcp-reads).
/// A named endpoint that does not answer is refused carrying that URL, before
/// any recipe is resolved or any byte downloaded: the restore probe cannot
/// judge a snapshot against an endpoint that is not there.
#[test]
fn pull_refuses_a_named_endpoint_that_does_not_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().to_str().unwrap();
    let r = run(
        tmp.path(),
        &[
            "pull",
            "--corpus",
            "sep",
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--data-dir",
            data,
        ],
    );
    assert_ne!(
        r.code, 0,
        "a pull with no endpoint succeeded:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("127.0.0.1:1"),
        "the refusal does not carry the URL that was named:\n{}",
        r.stderr
    );
    assert!(
        !tmp.path().join("indexes").join("sep").exists(),
        "a refused pull wrote an index"
    );
}

// ── `index` — code's chunk index, built by ingest (pb-code-index) ──────────

/// A one-file Rust repo and a code recipe over it, the shape `code index`
/// writes. `vector` is the recipe's `[index] vector`.
fn code_fixture(dir: &Path, vector: bool) -> std::path::PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn fixture_target() {}\n\npub fn fixture_caller() {\n    fixture_target();\n}\n",
    )
    .unwrap();
    let recipe = dir.join("fixture.toml");
    std::fs::write(
        &recipe,
        format!(
            "[corpus]\nid = \"fixture\"\nname = \"fixture\"\ndescription = \"test\"\n\
             license = \"private\"\nmesh_sharing = false\nsize_compressed_gb = 0\n\
             size_indexed_gb = 0\n\n[acquire]\ntype = \"local_file\"\npath = \"{}\"\n\n\
             [extract]\ntype = \"code\"\ncontext_lines = 3\nmax_lines_per_chunk = 150\n\n\
             [chunk]\ntype = \"passthrough\"\n\n[index]\nfts = true\nvector = {vector}\n",
            repo.display()
        ),
    )
    .unwrap();
    recipe
}

fn stamped_model(index_dir: &Path) -> String {
    let meta = std::fs::read_to_string(index_dir.join("fixture/_corpus_meta.json"))
        .expect("the build wrote no _corpus_meta.json");
    let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
    v["embedding_model"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// THE METADATA TEST. A keyword-only index holds zero vectors, so it must not
/// carry a model's name: a reader that matches the name would search zeros as
/// if they were that model's space (the `project init` defect, pb-code-index).
#[test]
fn an_fts_only_index_is_stamped_with_no_model_name() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe = code_fixture(tmp.path(), false);
    let index_dir = tmp.path().join("indexes");
    let r = run(
        tmp.path(),
        &[
            "index",
            "--recipe",
            recipe.to_str().unwrap(),
            "--index-dir",
            index_dir.to_str().unwrap(),
            "--fts-only",
        ],
    );
    assert_eq!(r.code, 0, "the keyword-only build failed:\n{}", r.stderr);
    let result: serde_json::Value = serde_json::from_str(r.stdout.lines().last().unwrap_or(""))
        .unwrap_or_else(|e| {
            panic!(
                "stdout's last line is not the JSON result ({e}):\n{}",
                r.stdout
            )
        });
    assert!(
        result["chunks_created"].as_u64().unwrap_or(0) > 0,
        "{result}"
    );
    assert_eq!(
        stamped_model(&index_dir),
        corpus_index::types::FTS_ONLY_EMBEDDING_MODEL,
        "a keyword-only index was stamped with a model name"
    );
}

/// The flag and the index agree or nothing is written: `--fts-only` against a
/// vector recipe would put zero vectors in a vector index, and a file-list run
/// with an embedder against a keyword-only index would mix the two. Both are
/// refused before any endpoint is probed.
#[test]
fn index_refuses_to_mix_keyword_only_and_vectors() {
    let tmp = tempfile::tempdir().unwrap();
    let vector_recipe = code_fixture(tmp.path(), true);
    let index_dir = tmp.path().join("indexes");
    let r = run(
        tmp.path(),
        &[
            "index",
            "--recipe",
            vector_recipe.to_str().unwrap(),
            "--index-dir",
            index_dir.to_str().unwrap(),
            "--fts-only",
        ],
    );
    assert_ne!(r.code, 0, "--fts-only built a vector recipe:\n{}", r.stderr);
    assert!(r.stderr.contains("vector = true"), "{}", r.stderr);
    assert!(
        !index_dir.join("fixture").exists(),
        "a refused build wrote an index"
    );

    // Build it keyword-only, then ask for a file-list run with an embedder.
    let recipe = code_fixture(tmp.path(), false);
    let args = [
        "index",
        "--recipe",
        recipe.to_str().unwrap(),
        "--index-dir",
        index_dir.to_str().unwrap(),
        "--fts-only",
    ];
    assert_eq!(run(tmp.path(), &args).code, 0);
    let list = tmp.path().join("changed.txt");
    std::fs::write(&list, "src/lib.rs\n").unwrap();
    let r = run(
        tmp.path(),
        &[
            "index",
            "--index-dir",
            index_dir.to_str().unwrap(),
            "--files-from",
            list.to_str().unwrap(),
            "--corpus",
            "fixture",
            "--root",
            tmp.path().join("repo").to_str().unwrap(),
        ],
    );
    assert_ne!(
        r.code, 0,
        "vectors went into a keyword-only index:\n{}",
        r.stderr
    );
    assert!(r.stderr.contains("none (fts-only)"), "{}", r.stderr);
    assert!(
        !r.stderr.contains("ollama"),
        "the refusal came after an endpoint probe:\n{}",
        r.stderr
    );
}

/// The file-list mode `code index --incremental` drives: a listed file is
/// re-indexed, and listing it again unchanged writes nothing and says so in
/// the JSON result.
#[test]
fn index_files_from_reindexes_only_the_listed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe = code_fixture(tmp.path(), false);
    let index_dir = tmp.path().join("indexes");
    let build = [
        "index",
        "--recipe",
        recipe.to_str().unwrap(),
        "--index-dir",
        index_dir.to_str().unwrap(),
        "--fts-only",
    ];
    assert_eq!(run(tmp.path(), &build).code, 0);

    let repo = tmp.path().join("repo");
    std::fs::write(repo.join("src/extra.rs"), "pub fn fixture_added() {}\n").unwrap();
    let list = tmp.path().join("changed.txt");
    std::fs::write(&list, "src/extra.rs\n").unwrap();
    let reindex = [
        "index",
        "--index-dir",
        index_dir.to_str().unwrap(),
        "--files-from",
        list.to_str().unwrap(),
        "--corpus",
        "fixture",
        "--root",
        repo.to_str().unwrap(),
        "--fts-only",
    ];
    let result = |r: &Run| -> serde_json::Value {
        assert_eq!(r.code, 0, "the file-list run failed:\n{}", r.stderr);
        serde_json::from_str(r.stdout.lines().last().unwrap_or("")).unwrap()
    };
    let first = result(&run(tmp.path(), &reindex));
    assert_eq!(first["updated"], 1, "{first}");
    assert_eq!(first["failed"], 0, "{first}");
    let again = result(&run(tmp.path(), &reindex));
    assert_eq!(again["unchanged"], 1, "{again}");
    assert_eq!(again["chunks_written"], 0, "{again}");
    assert_eq!(
        stamped_model(&index_dir),
        corpus_index::types::FTS_ONLY_EMBEDDING_MODEL
    );
}
