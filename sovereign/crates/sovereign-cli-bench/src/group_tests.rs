// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bench group's source censuses, moved from sovereign-cli-llm's lib.rs
//! with the trees they scan (pb-cli-llm-bench-move).

#[test]
fn eval_cmd_names_no_inner_chaos_module() {
    let needle = ["inner", "chaos"].join("_");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/eval_cmd");
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read eval_cmd").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            scanned += 1;
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains(&needle) {
                    offenders.push(format!("{}:{}", path.display(), i + 1));
                }
            }
        }
    }
    assert!(scanned > 10, "only {scanned} files scanned");
    assert!(
        offenders.is_empty(),
        "eval_cmd moves to bench and must not name svrn's `{needle}`: {offenders:?}"
    );
}

/// bench judges what svrn's probe (`svrn __probe`) reports and never runs
/// svrn's internal stages itself (phase-b-58). Comments are dropped and
/// whitespace squeezed out, so a call split across lines
/// (`session\n.runtime\n.router\n.classify(`) is still one match.
#[test]
fn eval_and_bench_run_no_svrn_internal_stage() {
    let needles = [
        ".router.classify(",
        ".retrieve_evidence(",
        ".search_with_rerank(",
        ".lane_sources",
    ];
    let (scanned, offenders) = bench_group_hits(&["eval_cmd", "bench_cmd"], &needles);
    assert!(scanned > 40, "only {scanned} files scanned");
    assert!(
        offenders.is_empty(),
        "a white-box stage runs in bench's own process; exec `svrn __probe` \
             instead: {offenders:?}"
    );
}

/// bench asks svrn a question as any client does and never drives a turn
/// in its own process (pb-bench-dials-turns). A file still owed to a
/// later row is listed with that row; a file off the list that matches
/// is red, and so is a listed file that no longer matches (its row
/// landed: drop it from the list).
#[test]
fn bench_group_drives_no_turn_in_process() {
    let needles = [
        "sovereign_core::runtime::Runtime",
        "collect_turn(",
        "build_session(",
        "build_session_sealed(",
        "build_session_with_skills(",
        "set_var(\"SOVEREIGN_RERANK",
        // An attached document is built and answered in svrn's probe
        // (`svrn __probe`, attached mode; pb-bench-dials-docs).
        "DocumentAssetManager::new(",
        // svrn's state store is read in svrn's probe (`svrn __probe`,
        // vault-build and raptor-nodes modes; pb-bench-dials-vault).
        "SqliteStateStore::open(",
    ];
    // Every row that owed a file here has landed (pb-bench-dials-vault
    // was the last); pb-cli-llm-bench-move needs it empty.
    const OWED: [(&str, &str); 0] = [];
    assert!(OWED.is_empty(), "the bench group owes no in-process svrn");
    // The three trees bench took (pb-cli-llm-bench-move). search_gym_cmd,
    // knowledge_gym_cmd and gym_judge stayed in sovereign-cli-llm as
    // svrn's own white-box lanes.
    let dirs = ["bench_cmd", "eval_cmd", "quality_lane_cmd"];
    let (scanned, hits) = bench_group_hits(&dirs, &needles);
    assert!(scanned > 60, "only {scanned} files scanned");
    let owed = |hit: &str| OWED.iter().any(|(f, _)| hit.contains(f));
    let unowed: Vec<&String> = hits.iter().filter(|h| !owed(h)).collect();
    assert!(
        unowed.is_empty(),
        "a bench lane drives a turn in its own process; ask svrn through \
             bench_cmd::subject::SubjectDial instead: {unowed:?}"
    );
    let paid: Vec<_> = OWED
        .iter()
        .filter(|(f, _)| !hits.iter().any(|h| h.contains(f)))
        .collect();
    assert!(
        paid.is_empty(),
        "no longer drives a turn in-process; drop from OWED: {paid:?}"
    );
}

/// Every `(file, needle)` hit under `dirs`, comments dropped and
/// whitespace squeezed out, plus the count of files scanned.
fn bench_group_hits(dirs: &[&str], needles: &[&str]) -> (usize, Vec<String>) {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    let mut stack: Vec<_> = dirs.iter().map(|d| src.join(d)).collect();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            scanned += 1;
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            let code: String = text
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .collect::<String>()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            for n in needles.iter().filter(|n| code.contains(*n)) {
                offenders.push(format!("{}: {n}", path.display()));
            }
        }
    }
    (scanned, offenders)
}
