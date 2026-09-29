// SPDX-License-Identifier: AGPL-3.0-or-later
//! The trailing-Judgement-line protocol lives in
//! `sovereign_contracts::lane_verdict` (phase-b pb-cli-llm-bench-move), the
//! leaf both the runner and bench's lanes admit; it is re-exported here at
//! its historical path. The one test that needs the checkout stays here.

pub use sovereign_contracts::lane_verdict::*;

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_types::Verdict;

    /// **THE TIE BETWEEN THE TWO IMPLEMENTATIONS.** The protocol is a wire
    /// format and a Python tool cannot call [`emit`], so
    /// `scripts/lib/judgement.py` mirrors it — one mirror rather than the nine
    /// `json.dumps` copies the alternative would have been. A mirror nothing
    /// checks is a mirror that drifts, so this parses the mirror's own
    /// `--selftest` output with the parser the runner actually uses, on both
    /// the happy path and the refusals: agreement on four well-formed lines
    /// says nothing about the strictness, which is the half that matters.
    #[test]
    fn the_python_emitter_speaks_the_same_protocol() {
        let root = crate::repo::find_checkout_root()
            .expect("this test runs inside the checkout that carries scripts/lib/judgement.py");
        let script = root.join("scripts/lib/judgement.py");
        assert!(
            script.is_file(),
            "{} is missing — the Python half of this protocol is gone, and \
             nine tools that emit a verdict line no longer can",
            script.display()
        );
        let out = std::process::Command::new("python3")
            .arg(&script)
            .arg("--selftest")
            .output()
            .expect(
                "python3 runs the emitter (it is a `binary:python3` precondition of this repo)",
            );
        assert!(out.status.success(), "selftest exited {}", out.status);
        let stdout = String::from_utf8(out.stdout).expect("utf-8");

        let mut seen: Vec<Verdict> = Vec::new();
        let mut refusals = 0usize;
        for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(rest) = line.strip_prefix("REFUSED ") {
                assert!(!rest.is_empty());
                refusals += 1;
                continue;
            }
            let j = parse_line(line).unwrap_or_else(|e| {
                panic!("the mirror emitted a line this parser rejects: {line} ({e})")
            });
            assert_eq!(j.subject(), "selftest");
            seen.push(j.verdict());
        }
        assert_eq!(
            seen,
            vec![
                Verdict::Passed,
                Verdict::Failed,
                Verdict::CouldNotJudge,
                Verdict::NeverRan
            ],
            "all four verdicts must cross the wire from Python too — two of \
             them are the ones an exit code cannot express"
        );
        // 3 verdict words outside the four, 5 reasons that carry nothing.
        assert_eq!(
            refusals, 8,
            "the mirror must refuse what `Verdict::parse_wire` and \
             `Reason::new` refuse; it reported {refusals} refusals"
        );
    }
}
