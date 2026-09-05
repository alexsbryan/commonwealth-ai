// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rec-1 fixture, shared by every ring. One plant per mechanism:
//! chain c1→c2→c3→c4 (linear depth 4) · `h.base` shared by f and g (memo)
//! · k compensates for the wrong `base` (merge trap) · cyc_a/cyc_b (occurs
//! check) · `net` reads an unset env var at import (could-not-judge).
//! `hints` adds `# BUG:` comments for ring 2; ring 3 runs without them.

#![allow(dead_code)]

use sovereign_tdd::recur::{Event, GoalId, GoalPath, StackState};
use sovereign_tdd::Workdir;
use std::path::Path;
use std::process::Command;

pub fn pytest_available() -> bool {
    Command::new("python3")
        .args(["-m", "pytest", "--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn sh(dir: &Path, args: &[&str]) {
    let out = Command::new(args[0])
        .args(&args[1..])
        .current_dir(dir)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// chain.py with the given bugs still planted. `c{i}` raises before it can
/// reach `c{i+1}`, so each bug masks the next: fixing c1 exposes c2.
pub fn chain_src(bugs: &[u8], hints: bool) -> String {
    let mut s = String::new();
    let hint = if hints {
        "  # BUG: delete this line"
    } else {
        ""
    };
    for i in (1..=4).rev() {
        let raise = if bugs.contains(&i) {
            format!("    raise ValueError(\"c{i} bug\"){hint}\n")
        } else {
            String::new()
        };
        let body = if i == 4 {
            "    return x * 4\n".to_string()
        } else {
            format!("    return c{}(x) + {i}\n", i + 1)
        };
        s.push_str(&format!("def c{i}(x):\n{raise}{body}\n"));
    }
    s
}

pub fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

/// The fixture. Root goal `tests` is red for five independent reasons.
pub fn fixture(root: &Path, hints: bool) -> Workdir {
    sh(root, &["git", "init", "-q", "--initial-branch=main"]);
    sh(root, &["git", "config", "user.email", "recur@test"]);
    sh(root, &["git", "config", "user.name", "recur"]);
    let h_hint = if hints {
        "  # BUG: should be a + b"
    } else {
        ""
    };
    write(root, ".gitignore", "__pycache__/\n.pytest_cache/\n");
    write(root, "conftest.py", "");
    write(root, "calc/__init__.py", "");
    write(root, "calc/chain.py", &chain_src(&[1, 2, 3, 4], hints));
    write(
        root,
        "calc/h.py",
        &format!("def base(a, b):\n    return a - b{h_hint}\n"),
    );
    write(
        root,
        "calc/f.py",
        "from .h import base\n\ndef f(x):\n    return base(x, 1) * 2\n",
    );
    write(
        root,
        "calc/g.py",
        "from .h import base\n\ndef g(x):\n    return base(x, 2) * 2\n",
    );
    write(
        root,
        "calc/k.py",
        "from .h import base\n\ndef k(x):\n    return base(x, 2) - 1\n",
    );
    write(
        root,
        "calc/cyc_a.py",
        "def a(x):\n    from .cyc_b import b\n    return b(x) + 1\n",
    );
    write(
        root,
        "calc/cyc_b.py",
        "def b(x):\n    from .cyc_a import a\n    return a(x) - 1\n",
    );
    write(
        root,
        "calc/net.py",
        "import os\nREMOTE = os.environ[\"CALC_REMOTE_RECUR_FIXTURE\"]\n",
    );
    write(
        root,
        "tests/test_chain.py",
        "from calc.chain import c1, c2, c3, c4\n\ndef test_c1(): assert c1(1) == 10\ndef test_c2(): assert c2(1) == 9\ndef test_c3(): assert c3(1) == 7\ndef test_c4(): assert c4(1) == 4\n",
    );
    write(
        root,
        "tests/test_top.py",
        "from calc.f import f\nfrom calc.g import g\nfrom calc.k import k\n\ndef test_f(): assert f(1) == 4\ndef test_g(): assert g(1) == 6\ndef test_k(): assert k(1) == 2\n",
    );
    write(
        root,
        "tests/test_h.py",
        "from calc.h import base\n\ndef test_base(): assert base(1, 1) == 2\n",
    );
    write(
        root,
        "tests/test_cycle.py",
        "from calc.cyc_a import a\nfrom calc.cyc_b import b\n\ndef test_cyc_a(): assert a(1) == 2\ndef test_cyc_b(): assert b(1) == 0\n",
    );
    write(
        root,
        "tests/test_net.py",
        "from calc.net import REMOTE\n\ndef test_net(): assert REMOTE\n",
    );
    sh(root, &["git", "add", "-A"]);
    sh(root, &["git", "commit", "-q", "-m", "fixture"]);
    Workdir::check_safe(root.to_path_buf(), false).unwrap()
}

pub fn g(s: &str) -> GoalId {
    GoalId::new(s)
}

pub fn root_path() -> GoalPath {
    GoalPath::root(g("tests"))
}

pub fn count<F: Fn(&Event) -> bool>(state: &StackState, f: F) -> usize {
    state.events.iter().filter(|e| f(e)).count()
}

/// Event log with the tempdir-specific worktree paths and memo keys
/// removed, so two fixtures compare equal.
pub fn strip_paths(state: &StackState) -> String {
    let s = serde_json::to_string_pretty(&state.events).unwrap();
    s.lines()
        .filter(|l| !l.contains("\"worktree\"") && !l.contains("\"key\""))
        .collect::<Vec<_>>()
        .join("\n")
}
