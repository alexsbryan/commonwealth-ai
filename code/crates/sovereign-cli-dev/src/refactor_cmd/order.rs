// SPDX-License-Identifier: AGPL-3.0-or-later
//! `next` and `close` — handing a worker one record, and proving what came
//! back.
//!
//! # The order must be sufficient
//!
//! The whole thesis (`quality/REFACTOR_LEDGER.md`, hypothesis L2) is that a
//! worker reading one order makes NO exploratory reads outside its file set.
//! That is why the order carries the sites with their current text, the rule
//! for the class, and worked examples of the same edit already landed — and why
//! **no order may cite a document**. Every reference an agent has to go and
//! open is a discovery cost it will either pay or silently skip, and both are
//! failures.
//!
//! # Two mechanisms, named separately
//!
//! The design wanted "no two agents in one file". The work atlas cannot supply
//! it: `declare_scope` mints a fresh claim id per call, so two callers claiming
//! one path write different keys and both succeed, and `MeshStore::set` is
//! last-writer-wins with no compare-and-swap. The crate says so deliberately —
//! *"visibility and graduated response, not a lock manager"*.
//!
//! So this module does the locking itself, with `O_EXCL` per file and rollback
//! on the first collision, and leaves cross-mesh VISIBILITY to the atlas. The
//! lock is local to this machine and says so rather than implying a guarantee
//! it has not got.
//!
//! # Closure is proved, not asserted
//!
//! `close` never takes the worker's word. It re-runs the detector, checks the
//! control fired, post-filters to the order's files, and reports what the
//! detector can no longer see. A holding the detector still matches stays open
//! no matter what the worker believes — which is why a half-finished order is
//! not a corrupt state, and why a dead session needs no reconciliation.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use super::detector::{self, DetectorCtx, DetectorId, Site};
use super::labels::{Disposition, LabelStore};

/// Where order files live. Gitignored — an order is a rendered view, not a
/// record, and it is regenerable from the ledger at any time.
const FEATURES_DIR: &str = ".sovereign/features";
/// Where per-file locks live.
const LOCK_DIR: &str = ".sovereign/refactor-locks";

/// How many sites one order may carry.
///
/// A declared estimate, not a measurement — printed with its basis wherever it
/// produces a number (ARCH §18.4). The first closed order replaces it.
pub const DEFAULT_BATCH: usize = 25;

/// An exclusive hold on a set of files, released on drop.
///
/// Multi-file locking with rollback: every file is claimed with `O_EXCL`, and
/// the first collision unwinds the ones already taken. Without the rollback a
/// refused order would leave half the tree locked by a process that then exits.
#[derive(Debug)]
pub struct FileLock {
    held: Vec<PathBuf>,
}

impl FileLock {
    pub fn acquire(root: &Path, order_id: &str, files: &[String]) -> Result<FileLock, String> {
        let dir = root.join(LOCK_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut held = Vec::new();
        for f in files {
            let path = dir.join(format!("{}.lock", f.replace('/', "%2F")));
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
            {
                Ok(mut fh) => {
                    use std::io::Write as _;
                    let _ = writeln!(fh, "{order_id}");
                    held.push(path);
                }
                Err(_) => {
                    // Unwind before refusing — see the type doc.
                    for p in &held {
                        let _ = std::fs::remove_file(p);
                    }
                    let owner = std::fs::read_to_string(&path).unwrap_or_default();
                    return Err(format!("{f} is already held by order {}", owner.trim()));
                }
            }
        }
        Ok(FileLock { held })
    }

    /// A lock holding nothing — used when ownership is deliberately handed to
    /// the filesystem so `close` can release it in a later process.
    pub fn empty() -> FileLock {
        FileLock { held: Vec::new() }
    }

    pub fn release(&mut self) {
        for p in self.held.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Release the lock a previous `next` took, by order id.
    pub fn release_for(root: &Path, order_id: &str) -> usize {
        let dir = root.join(LOCK_DIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return 0;
        };
        let mut n = 0;
        for e in entries.flatten() {
            let p = e.path();
            if std::fs::read_to_string(&p)
                .map(|s| s.trim() == order_id)
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(&p);
                n += 1;
            }
        }
        n
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// One batch, chosen and rendered.
pub struct Order {
    pub id: String,
    pub detector: DetectorId,
    pub destination: String,
    pub sites: Vec<Site>,
    pub files: Vec<String>,
    pub settings_digest: String,
}

/// Pick the next batch: one detector, one destination, file-disjoint.
///
/// Rule-homogeneous on purpose — a worker that learns one edit and repeats it
/// twenty-five times is doing something very different from one that context
/// switches twenty-five times.
pub fn choose(
    sites_by_detector: &[(DetectorId, String, Vec<Site>)],
    store: &LabelStore,
    batch: usize,
) -> Option<Order> {
    let mut best: Option<Order> = None;
    for (id, digest, sites) in sites_by_detector {
        // Group this detector's converge-labelled sites by destination.
        let mut by_dest: std::collections::BTreeMap<String, Vec<Site>> = Default::default();
        for s in sites {
            let Some(l) = store.get(&s.key()) else {
                continue;
            };
            if l.disp != Disposition::Converge {
                continue;
            }
            by_dest.entry(l.dest.clone()).or_default().push(s.clone());
        }
        for (dest, mut group) in by_dest {
            group.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
            group.truncate(batch);
            let mut files: Vec<String> = group.iter().map(|s| s.file.clone()).collect();
            files.sort();
            files.dedup();
            let candidate = Order {
                id: order_id(*id, &dest),
                detector: *id,
                destination: dest,
                sites: group,
                files,
                settings_digest: digest.clone(),
            };
            if best
                .as_ref()
                .is_none_or(|b| candidate.sites.len() > b.sites.len())
            {
                best = Some(candidate);
            }
        }
    }
    best
}

/// A stable, readable id — never a counter or a timestamp (ARCH §7.5:
/// identity from essence).
fn order_id(detector: DetectorId, dest: &str) -> String {
    let slug: String = dest
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-').replace("--", "-");
    format!("rf-{}-{}", detector.as_str(), slug)
}

/// Render `work-order/v1`.
///
/// Written directly rather than through `scripts/co-order.sh new`: that script
/// emits a hand-drafting template whose body this would immediately overwrite,
/// and the FORMAT is the contract, not the script. `co-order.sh close` still
/// works on the result — it is a regex rewrite of the `status:` line.
pub fn render(order: &Order, examples: &[String], root: &Path) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "---");
    let _ = writeln!(o, "schema: work-order/v1");
    let _ = writeln!(o, "id: {}", order.id);
    let _ = writeln!(o, "status: open");
    let _ = writeln!(o, "approved: generated by `svrn code refactor next`");
    let _ = writeln!(o, "serves: refactor-factory");
    let _ = writeln!(o, "campaign: refactor-factory");
    let _ = writeln!(o, "lane: burn one ledger record to zero");
    let _ = writeln!(o, "budget: 1 session-chunk");
    let _ = writeln!(o, "detector: {}", order.detector.as_str());
    let _ = writeln!(o, "settings_digest: {}", order.settings_digest);
    let _ = writeln!(o, "---");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "# Order: {} — {} sites",
        order.destination,
        order.sites.len()
    );
    let _ = writeln!(o);
    let _ = writeln!(o, "## Objective");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Coerce every site below onto `{}`. Nothing else.",
        order.destination
    );
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Done when: `svrn code refactor close {}` reports every site closed.",
        order.id
    );
    let _ = writeln!(
        o,
        "You do NOT mark anything done — the detector decides, by no longer"
    );
    let _ = writeln!(
        o,
        "matching. A site you edited that still matches is still open, and that"
    );
    let _ = writeln!(o, "is information, not a failure.");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Not worth continuing if: more than a third of the sites need a"
    );
    let _ = writeln!(
        o,
        "semantic decision rather than the mechanical edit. Escalate instead."
    );
    let _ = writeln!(o);

    let _ = writeln!(o, "## Sites");
    let _ = writeln!(o);
    for s in &order.sites {
        let _ = writeln!(o, "- `{}:{}` — {}", s.file, s.line, s.note);
        if let Some(text) = line_text(root, &s.file, s.line) {
            let _ = writeln!(o, "  ```rust");
            let _ = writeln!(o, "  {}", text.trim());
            let _ = writeln!(o, "  ```");
        }
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## Worked examples");
    let _ = writeln!(o);
    if examples.is_empty() {
        // Absence reported, never defaulted (ARCH §18.3). An order with no
        // precedent is a FIRST of its kind and the worker should know.
        let _ = writeln!(
            o,
            "None — no commit yet carries `Refactor-Rule: {}/{}`.",
            order.detector.as_str(),
            order.destination
        );
        let _ = writeln!(
            o,
            "This is the first order of its class, so there is no landed edit to"
        );
        let _ = writeln!(o, "copy. Work more slowly and escalate anything ambiguous.");
    } else {
        let _ = writeln!(o, "The same edit, already landed and passing:");
        let _ = writeln!(o);
        for e in examples {
            let _ = writeln!(o, "- {e}");
        }
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## Scope");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "These files, and no others. They are locked to this order:"
    );
    let _ = writeln!(o);
    for f in &order.files {
        let _ = writeln!(o, "- `{f}`");
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "## If a site will not go");
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Do not guess. Leave it, finish the rest, and say which site and why in"
    );
    let _ = writeln!(
        o,
        "your close report. One ambiguous site must never block the other {}.",
        order.sites.len().saturating_sub(1)
    );
    o
}

fn line_text(root: &Path, file: &str, line: i32) -> Option<String> {
    if line <= 0 {
        return None;
    }
    let text = std::fs::read_to_string(root.join(file)).ok()?;
    text.lines().nth((line - 1) as usize).map(str::to_string)
}

/// Worked examples from git — commits carrying this rule's trailer.
///
/// Nothing is stored: the record of "this edit has been made before, and it
/// held" already lives in the history. Matches the house trailer style
/// (`Gates:`, `Verified:`).
pub fn worked_examples(root: &Path, detector: DetectorId, dest: &str) -> Vec<String> {
    let needle = format!("Refactor-Rule: {}/{}", detector.as_str(), dest);
    let Ok(out) = std::process::Command::new("git")
        .args(["log", "--grep", &needle, "--pretty=format:%h %s", "-n", "3"])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

pub fn order_path(root: &Path, id: &str) -> PathBuf {
    root.join(FEATURES_DIR).join(id).join("order.md")
}

/// The machine half of an order, beside the human half.
///
/// `order.md` is for the worker; this is what `close` reads back to know which
/// sites the order was cut from. Kept separate so the document stays readable
/// and so nothing tempts anyone to parse prose.
pub fn sites_path(root: &Path, id: &str) -> PathBuf {
    root.join(FEATURES_DIR).join(id).join("sites.json")
}

pub fn write_order(root: &Path, order: &Order, body: &str) -> Result<(), String> {
    let dir = root.join(FEATURES_DIR).join(&order.id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(order_path(root, &order.id), body).map_err(|e| format!("writing order: {e}"))?;
    let json = serde_json::to_string_pretty(&order.sites).map_err(|e| e.to_string())?;
    std::fs::write(sites_path(root, &order.id), json).map_err(|e| format!("writing sites: {e}"))?;
    Ok(())
}

pub fn read_sites(root: &Path, id: &str) -> Result<Vec<Site>, String> {
    let path = sites_path(root, id);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: {e} — is `{id}` an order this host cut?",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// What `close` proved.
pub struct CloseReport {
    pub order_id: String,
    pub detector: DetectorId,
    pub control_live: bool,
    pub control_reason: String,
    pub closed: Vec<Site>,
    pub still_open: Vec<Site>,
}

/// Re-run the detector and report what it can no longer see.
///
/// `before` is the site list the order was cut from. Anything in `before` that
/// the fresh run no longer returns is CLOSED — proved by the instrument, not
/// claimed by the worker.
pub async fn prove(
    ctx: &DetectorCtx<'_>,
    order_id: &str,
    detector: &dyn detector::Detector,
    before: &[Site],
    files: &[String],
) -> Result<CloseReport, String> {
    let report = detector.fire(ctx).await?;
    let after = Site::in_files(&report.sites, files);
    let after_keys: std::collections::HashSet<String> = after.iter().map(Site::key).collect();

    let (still_open, closed): (Vec<Site>, Vec<Site>) = before
        .iter()
        .cloned()
        .partition(|s| after_keys.contains(&s.key()));

    let live = report.control.verdict() == kernel_types::Verdict::Passed;
    Ok(CloseReport {
        order_id: order_id.to_string(),
        detector: detector.id(),
        control_live: live,
        control_reason: report.control.reason().as_str().to_string(),
        // A dead control closes NOTHING, however much the diff looks right.
        closed: if live { closed } else { Vec::new() },
        still_open: if live { still_open } else { before.to_vec() },
    })
}

pub fn render_close(r: &CloseReport) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "  order      {}", r.order_id);
    let _ = writeln!(o, "  detector   {}", r.detector.as_str());
    if r.control_live {
        let _ = writeln!(o, "  control    fired — instrument is live");
    } else {
        let _ = writeln!(o, "  control    SILENT — {}", r.control_reason);
        let _ = writeln!(
            o,
            "  CLOSED NOTHING. Settle the control before trusting this run."
        );
    }
    let _ = writeln!(o, "  closed     {}", r.closed.len());
    let _ = writeln!(o, "  still open {}", r.still_open.len());
    for s in r.still_open.iter().take(10) {
        let _ = writeln!(o, "    {}:{} — still matches", s.file, s.line);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(file: &str, token: &str) -> Site {
        Site {
            detector: DetectorId::Name,
            file: file.to_string(),
            line: 1,
            locus: file.to_string(),
            token: token.to_string(),
            note: "n".into(),
        }
    }

    #[test]
    fn a_lock_is_exclusive_and_names_its_owner() {
        let d = tempfile::tempdir().unwrap();
        let files = vec!["a.rs".to_string()];
        let _held = FileLock::acquire(d.path(), "rf-1", &files).unwrap();
        let err = FileLock::acquire(d.path(), "rf-2", &files).unwrap_err();
        assert!(err.contains("already held by order rf-1"), "{err}");
    }

    /// The rollback. Without it a refused order leaves the files it managed to
    /// take locked forever by a process that has exited.
    #[test]
    fn a_collision_unwinds_the_files_already_taken() {
        let d = tempfile::tempdir().unwrap();
        let _first = FileLock::acquire(d.path(), "rf-1", &["b.rs".to_string()]).unwrap();
        // rf-2 wants a.rs (free) then b.rs (held) — it must give a.rs back.
        let err = FileLock::acquire(d.path(), "rf-2", &["a.rs".to_string(), "b.rs".to_string()])
            .unwrap_err();
        assert!(err.contains("b.rs"), "{err}");
        FileLock::acquire(d.path(), "rf-3", &["a.rs".to_string()])
            .expect("a.rs must have been released by the rollback");
    }

    #[test]
    fn a_lock_releases_on_drop() {
        let d = tempfile::tempdir().unwrap();
        {
            let _l = FileLock::acquire(d.path(), "rf-1", &["a.rs".to_string()]).unwrap();
        }
        FileLock::acquire(d.path(), "rf-2", &["a.rs".to_string()]).expect("released on drop");
    }

    #[test]
    fn an_order_id_comes_from_the_destination_never_a_counter() {
        let a = order_id(DetectorId::Name, "kernel_types::Verdict");
        assert_eq!(a, order_id(DetectorId::Name, "kernel_types::Verdict"));
        assert!(a.starts_with("rf-name-"), "{a}");
        assert!(!a.contains("::"), "{a}");
    }

    #[test]
    fn an_order_with_no_precedent_says_so_rather_than_showing_an_empty_section() {
        let d = tempfile::tempdir().unwrap();
        let order = Order {
            id: "rf-name-x".into(),
            detector: DetectorId::Name,
            destination: "kernel_types::Verdict".into(),
            sites: vec![site("a.rs", "Verdict")],
            files: vec!["a.rs".into()],
            settings_digest: "t".into(),
        };
        let out = render(&order, &[], d.path());
        assert!(out.contains("first order of its class"), "{out}");
    }

    #[test]
    fn the_order_never_tells_the_worker_to_go_read_a_document() {
        let d = tempfile::tempdir().unwrap();
        let order = Order {
            id: "rf-name-x".into(),
            detector: DetectorId::Name,
            destination: "kernel_types::Verdict".into(),
            sites: vec![site("a.rs", "Verdict")],
            files: vec!["a.rs".into()],
            settings_digest: "t".into(),
        };
        let out = render(&order, &["abc123 did the thing".into()], d.path());
        // L2: every doc reference is a discovery cost the agent pays or skips.
        assert!(
            !out.contains(".md"),
            "an order must be self-contained: {out}"
        );
    }

    #[test]
    fn the_order_carries_the_frozen_settings_so_a_close_can_check_them() {
        let d = tempfile::tempdir().unwrap();
        let order = Order {
            id: "rf-name-x".into(),
            detector: DetectorId::Name,
            destination: "D".into(),
            sites: vec![site("a.rs", "V")],
            files: vec!["a.rs".into()],
            settings_digest: "threshold=0.5".into(),
        };
        let out = render(&order, &[], d.path());
        assert!(out.contains("settings_digest: threshold=0.5"), "{out}");
    }
}
