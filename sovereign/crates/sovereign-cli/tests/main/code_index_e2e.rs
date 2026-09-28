// SPDX-License-Identifier: AGPL-3.0-or-later
//! pb-code-index's proof: indexing a project needs the code program
//! (`sovereign-cli-dev`) and ingest's CLI (`svrn-ingest`), never a daemon.
//! `svrn init` then `svrn code index --fts-only` run on a fixture repo with
//! every daemon URL at a dead port and no embedder anywhere; init's banner
//! names the keyword-only index, its metadata carries no model name, and
//! `code_search` answers from it.
//!
//! The two siblings are the ones beside this test's dispatcher; a missing one
//! fails by name (the `distribute_e2e` precedent), never skips.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_sovereign-cli");
/// Nothing listens on loopback port 1: a dial is refused at once.
const DEAD: &str = "http://127.0.0.1:1";

fn sibling(name: &str, package: &str) -> PathBuf {
    let path = Path::new(BIN).with_file_name(name);
    assert!(
        path.is_file(),
        "{} is missing: build it with `cargo build -p {package}`",
        path.display()
    );
    path
}

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    /// A committed one-crate git repo and an empty home.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("repo dir");
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("fixture manifest");
        std::fs::write(
            repo.join("src/lib.rs"),
            "pub fn fixture_target() {}\n\npub fn fixture_caller() {\n    fixture_target();\n}\n",
        )
        .expect("fixture source");
        std::fs::write(repo.join(".gitignore"), ".sovereign/\n").expect("fixture .gitignore");
        for args in [
            &["init", "-q"][..],
            &["add", "-A"][..],
            &[
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "-m",
                "fixture",
            ][..],
        ] {
            let git = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .expect("run git");
            assert!(git.success(), "git {args:?}");
        }
        std::fs::create_dir_all(dir.path().join("home")).expect("home dir");
        Self { dir }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn svrn(&self, args: &[&str]) -> Output {
        let home = self.path("home");
        Command::new(BIN)
            .args(args)
            .current_dir(self.path("repo"))
            .stdin(Stdio::null())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("SVRNMESH_DATA_DIR", home.join(".svrnmesh"))
            .env_remove("SOVEREIGN_DATA_DIR")
            .env("SVRNMESH_DAEMON_URL", DEAD)
            .env("SOVEREIGN_DAEMON_URL", DEAD)
            .env(
                "SOVEREIGN_CLI_DEV_BIN",
                sibling("sovereign-cli-dev", "sovereign-cli-dev"),
            )
            .env(
                "SOVEREIGN_INGEST_BIN",
                sibling("svrn-ingest", "sovereign-pipeline"),
            )
            .output()
            .expect("spawn sovereign-cli")
    }

    fn meta(&self) -> serde_json::Value {
        let path = self.path("home/.svrnmesh/indexes/repo/_corpus_meta.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} was not written: {e}", path.display()));
        serde_json::from_str(&text).expect("_corpus_meta.json is JSON")
    }
}

fn text(out: &Output) -> String {
    format!(
        "status: {:?}\n--- stdout\n{}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn init_then_code_index_need_no_daemon_and_code_search_answers_from_full_text() {
    let sb = Sandbox::new();

    let init = sb.svrn(&[
        "init",
        "--no-serve",
        "--no-scip",
        "--no-hooks",
        "--no-claude-config",
    ]);
    assert!(init.status.success(), "svrn init failed:\n{}", text(&init));
    assert!(
        text(&init).contains("Keyword-only (FTS)"),
        "init's banner does not name the keyword-only index:\n{}",
        text(&init)
    );
    assert_eq!(
        sb.meta()["embedding_model"],
        "none (fts-only)",
        "init stamped a model name on a keyword-only index"
    );

    // `code_search` searches code corpora, which it tells apart by their SCIP
    // graph. `--no-scip` skipped that export (it needs a language toolchain
    // this test does not assume), so an empty graph stands in for it. The
    // chunk index the search reads is init's own.
    let graph = sb.path("home/.svrnmesh/indexes/repo/scip_graph.db");
    corpus_engine_scip::ScipGraph::open(&graph, "repo").expect("an empty SCIP graph");

    // An edit, then the code program's own verb, still with no embedder.
    std::fs::write(
        sb.path("repo/src/extra.rs"),
        "pub fn fixture_added_after_init() {}\n",
    )
    .expect("fixture edit");
    let index = sb.svrn(&["code", "index", ".", "--fts-only"]);
    assert!(
        index.status.success(),
        "svrn code index --fts-only failed:\n{}",
        text(&index)
    );
    assert_eq!(sb.meta()["embedding_model"], "none (fts-only)");

    let search = sb.svrn(&[
        "tools",
        "call",
        "code_search",
        "--query=fixture_added_after_init",
    ]);
    let out = text(&search);
    assert!(search.status.success(), "code_search failed:\n{out}");
    assert!(
        out.contains("extra.rs:"),
        "code_search returned no row from the file indexed after init:\n{out}"
    );

    // With neither `--fts-only` nor an embedder, a rebuild is refused by
    // name, never built on zero vectors under some model's name. Last,
    // because a refused rebuild has already cleared the chunk table.
    let refused = sb.svrn(&["code", "index", ".", "--full"]);
    assert!(
        !refused.status.success(),
        "a no-embedder rebuild succeeded:\n{}",
        text(&refused)
    );
    assert!(
        text(&refused).contains("--fts-only"),
        "the refusal does not name --fts-only:\n{}",
        text(&refused)
    );
}
