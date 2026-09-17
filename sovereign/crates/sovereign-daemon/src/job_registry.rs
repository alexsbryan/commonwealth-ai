// SPDX-License-Identifier: AGPL-3.0-or-later
//! The table every long-running daemon route keeps its jobs in.
//!
//! Five routes here had grown the same five lines —
//! `OnceLock<Mutex<HashMap<String, Arc<Job>>>>`, a `get_or_init` accessor, a
//! `get(key).cloned()` lookup and an `insert` — around five different job
//! payloads (`corpus_catalog_http::IndexBuild`, `lc_http::ClusterJob`,
//! `research_http::ResearchJob`, `documents_http::{DocumentJob, AskJob}`).
//! The payloads differ for real reasons: a percentage and an outcome, a frame
//! log, a run directory and a stage. The TABLE does not, and a sixth copy is
//! what this module exists to not write (ARCH principle 8).
//!
//! # What the shared form fixes, not just shortens
//!
//! `lc_http::cluster_job_for` already answered `Result<Option<_>, ()>` —
//! **a poisoned table is a different fact from "no such job"**, and reporting
//! the first as the second tells a poller its job vanished when what actually
//! happened is that a worker panicked holding the lock (ARCH principle 6).
//! The other four collapsed both into `None` with `.ok()`. [`JobRegistry::get`]
//! makes the honest answer the only one available: the collapse is a
//! deliberate `.ok()` at a call site now, not the path of least resistance.
//!
//! In-process on purpose, like every table it replaces: the work these jobs
//! narrate is this daemon's, and a log that outlived the daemon would describe
//! a run that did not finish.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The table is poisoned — a task panicked while holding the lock.
///
/// Deliberately its own type rather than `()`: a caller that wants to treat it
/// as "no job" has to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TablePoisoned;

impl std::fmt::Display for TablePoisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the job table is poisoned (a worker panicked holding it)"
        )
    }
}

/// A keyed table of live (and recently finished) in-process jobs.
///
/// `J` is the route's own job payload — whatever it needs to answer its
/// progress route. This owns only the table.
///
/// Declared as a `static` beside the route that fills it:
///
/// ```ignore
/// static INDEX_BUILDS: JobRegistry<IndexBuild> = JobRegistry::new("index_build");
/// ```
pub struct JobRegistry<J> {
    table: OnceLock<Mutex<HashMap<String, Arc<J>>>>,
    /// What these jobs are, for the tracing events below. A registry that
    /// could not name itself would make every warning identical.
    kind: &'static str,
}

impl<J> JobRegistry<J> {
    /// A new, empty registry. `const` so it can be a `static` with no
    /// lazy-init dance at each call site.
    pub const fn new(kind: &'static str) -> Self {
        Self {
            table: OnceLock::new(),
            kind,
        }
    }

    fn table(&self) -> &Mutex<HashMap<String, Arc<J>>> {
        self.table.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// The job on record for `key`.
    ///
    /// `Ok(None)` is "nobody asked for this one"; `Err(TablePoisoned)` is "the
    /// table cannot be read". The two render differently to a poller and must
    /// not collapse.
    pub fn get(&self, key: &str) -> Result<Option<Arc<J>>, TablePoisoned> {
        match self.table().lock() {
            Ok(jobs) => Ok(jobs.get(key).cloned()),
            Err(_) => {
                tracing::warn!(kind = self.kind, key, "job_registry:table_poisoned");
                Err(TablePoisoned)
            }
        }
    }

    /// Put `job` on record under `key`, replacing any earlier one.
    ///
    /// Returns whether it landed. A poisoned table loses the record — said on
    /// the trace rather than swallowed, because the progress route will then
    /// answer "no such job" for a job that IS running.
    pub fn insert(&self, key: String, job: Arc<J>) -> bool {
        match self.table().lock() {
            Ok(mut jobs) => {
                jobs.insert(key, job);
                true
            }
            Err(_) => {
                tracing::warn!(
                    kind = self.kind,
                    key,
                    "job_registry:insert_lost — table poisoned, this job will not be findable"
                );
                false
            }
        }
    }

    /// Put `job` on record under `key` UNLESS one already there is still
    /// live. `Ok(Some(live))` is the refusal — the caller renders the 409 and
    /// can name the running job; `Ok(None)` means this one landed.
    ///
    /// The check and the insert happen under ONE lock, which is the point: a
    /// `get` followed by an `insert` lets two concurrent requests both find
    /// the slot empty, and for a route whose whole refusal is "two writers on
    /// one index is worse than a refusal" that race is the bug.
    pub fn insert_unless_live(
        &self,
        key: String,
        job: Arc<J>,
        is_live: impl Fn(&J) -> bool,
    ) -> Result<Option<Arc<J>>, TablePoisoned> {
        match self.table().lock() {
            Ok(mut jobs) => {
                if let Some(live) = jobs.get(&key) {
                    if is_live(live) {
                        return Ok(Some(Arc::clone(live)));
                    }
                }
                jobs.insert(key, job);
                Ok(None)
            }
            Err(_) => {
                tracing::warn!(kind = self.kind, key, "job_registry:table_poisoned");
                Err(TablePoisoned)
            }
        }
    }

    /// Every job on record, in unspecified order.
    ///
    /// For the routes that report a LIST (what is this daemon driving right
    /// now). Returns owned handles so the table lock is not held while the
    /// caller reads each job.
    pub fn snapshot(&self) -> Result<Vec<Arc<J>>, TablePoisoned> {
        match self.table().lock() {
            Ok(jobs) => Ok(jobs.values().cloned().collect()),
            Err(_) => {
                tracing::warn!(kind = self.kind, "job_registry:table_poisoned");
                Err(TablePoisoned)
            }
        }
    }

    /// The first job matching `pred`, in unspecified order.
    ///
    /// The "one run at a time" deciders use this over the whole table.
    pub fn find(&self, pred: impl Fn(&J) -> bool) -> Result<Option<Arc<J>>, TablePoisoned> {
        match self.table().lock() {
            Ok(jobs) => Ok(jobs.values().find(|j| pred(j)).cloned()),
            Err(_) => {
                tracing::warn!(kind = self.kind, "job_registry:table_poisoned");
                Err(TablePoisoned)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Job(u32);

    static JOBS: JobRegistry<Job> = JobRegistry::new("test");

    /// The two absences a poller can face are different answers.
    ///
    /// Watched fail: change `get` to return `Ok(None)` on a poisoned lock and
    /// the second half goes green on a table nobody can read — which is the
    /// exact collapse four of the five hand-rolled copies shipped.
    #[test]
    fn a_missing_job_and_an_unreadable_table_are_different_answers() {
        static POISONED: JobRegistry<Job> = JobRegistry::new("poisoned");

        assert_eq!(
            JOBS.get("nobody").map(|o| o.is_none()),
            Ok::<_, TablePoisoned>(true)
        );
        JOBS.insert("a".into(), Arc::new(Job(7)));
        assert_eq!(JOBS.get("a").unwrap().map(|j| j.0), Some(7));

        // Poison it for real: panic inside the lock.
        let _ = std::panic::catch_unwind(|| {
            let _g = POISONED.table().lock().unwrap();
            panic!("worker died holding the table");
        });
        assert_eq!(POISONED.get("a").err(), Some(TablePoisoned));
        assert!(!POISONED.insert("b".into(), Arc::new(Job(1))));
    }

    /// `find` and `snapshot` scan the table, which is what the "one run at a
    /// time" decider and the "what is running" route need, and both report a
    /// poisoned table rather than "no live run" — the answer that would let a
    /// second run start.
    #[test]
    fn find_and_snapshot_scan_the_table() {
        static SCAN: JobRegistry<Job> = JobRegistry::new("scan");
        SCAN.insert("x".into(), Arc::new(Job(1)));
        SCAN.insert("y".into(), Arc::new(Job(2)));
        assert!(SCAN.find(|j| j.0 == 2).unwrap().is_some());
        assert!(SCAN.find(|j| j.0 == 99).unwrap().is_none());
        let mut seen: Vec<u32> = SCAN.snapshot().unwrap().iter().map(|j| j.0).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2]);
    }

    /// A second job on a key whose job is still live is REFUSED and the
    /// caller is handed the live one to name; a finished job is replaced.
    ///
    /// Watched fail: implement this as `get` then `insert` and the refusal
    /// still passes here — which is why the doc names the race and the
    /// single-lock body is the thing being pinned, not the two outcomes.
    #[test]
    fn a_live_job_refuses_its_replacement_and_a_finished_one_does_not() {
        static SLOT: JobRegistry<Job> = JobRegistry::new("slot");
        assert!(SLOT
            .insert_unless_live("k".into(), Arc::new(Job(1)), |_| true)
            .unwrap()
            .is_none());
        let refused = SLOT
            .insert_unless_live("k".into(), Arc::new(Job(2)), |_| true)
            .unwrap()
            .expect("a live job refuses");
        assert_eq!(refused.0, 1, "the caller is handed the LIVE job to name");
        assert_eq!(
            SLOT.get("k").unwrap().unwrap().0,
            1,
            "and nothing replaced it"
        );

        assert!(SLOT
            .insert_unless_live("k".into(), Arc::new(Job(3)), |_| false)
            .unwrap()
            .is_none());
        assert_eq!(SLOT.get("k").unwrap().unwrap().0, 3);
    }
}
