//! Window lineage — which session a new session is the CONTINUATION of.
//!
//! THE PROBLEM THIS SOLVES (measured 2026-07-27 on RuggedFox, 50 live session
//! dirs). `/clear` mints a brand-new `session_id`. It is not a resume, so the
//! `own_full` boot path never fires — 20 of 22 boot records on this machine are
//! `source: clear`, every one of them `frame_is_own: false`. The successor
//! therefore had to *guess* its predecessor out of 25–42 candidate frames, and
//! with one long-lived branch every candidate scores `branch_match: true`, so
//! the guess collapsed onto prompt-overlap junk ("everything", "continue",
//! "solution") and then recency. With two terminals open, "newest" is the OTHER
//! window's frame about half the time.
//!
//! It cost a real restart penalty 4 minutes before this module was written:
//! session `963fc519` — the `/clear` successor of `a05e2bd1` in the same
//! terminal — was handed the F9-scheduler frame `ad5fee8c`, said out loud
//! "wrong arc", and hand-ran `sovereign session frames a05e2bd1` to fetch the
//! frame its own predecessor had banked 9 minutes earlier.
//!
//! THE KEY INSIGHT. `/clear` does not restart the harness process. Proved from
//! the same data: `claude` pid 2446063 started 20:47:47 (= boot of `a05e2bd1`)
//! and was still alive at 21:17:47 when the clear minted `963fc519`. So the
//! owning harness process is a STABLE IDENTITY for the window across clears,
//! and DISTINCT per concurrent window. That turns "which frame?" from a lexical
//! guess into a lookup: the predecessor is whoever last occupied this window.
//!
//! The identity is (pid, process start time). The start time is what makes it
//! safe: a recycled pid belonging to a different process resolves to a
//! different key, so a stale pointer can never be mis-attributed to a new
//! window — it simply never matches again.
//!
//! DEGRADATION. Every failure here — no `ps`, no harness ancestor (running from
//! a plain shell, CI, `claude -p`), unreadable pointer — returns `None` and the
//! caller falls back to the ranked index exactly as before. Lineage is a
//! shortcut past a guess, never a prerequisite for one.

use std::path::{Path, PathBuf};

/// How far up the process tree to look for the harness. The observed chain is
/// short (`sovereign` ← `python3` ← `sh` ← `claude`); 8 is slack for wrappers.
const MAX_HOPS: usize = 8;

/// Pointers older than this are pruned on write. A window idle for two weeks
/// is not a window anyone is continuing.
const POINTER_MAX_AGE_DAYS: u64 = 14;

/// Read a knob under either prefix. `SOVEREIGN_*` is deprecated in favour of
/// `SVRNMESH_*` (`sovereign_contracts::rebrand`), and new vars should not add
/// to that debt — but the bridge only runs inside binaries that call it, so
/// accepting both here is what makes these usable from a plain shell.
pub(crate) fn env_either(suffix: &str) -> Option<String> {
    for prefix in ["SVRNMESH_", "SOVEREIGN_"] {
        if let Ok(v) = std::env::var(format!("{prefix}{suffix}")) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// The harness process that owns this terminal. `comm` is matched, not the
/// full argv, so a `claude` launched by any path resolves the same.
fn harness_comms() -> Vec<String> {
    match env_either("HARNESS_COMM") {
        Some(v) => v.split(',').map(|s| s.trim().to_string()).collect(),
        None => vec!["claude".to_string()],
    }
}

/// The identity of one harness window.
#[derive(Debug, Clone)]
pub(crate) struct WindowKey {
    pub(crate) pid: u32,
    /// `ps -o lstart=` text, e.g. `Mon Jul 27 21:20:09 2026`. Carried verbatim
    /// for the human view; hashed into `key` so the filename stays tidy.
    pub(crate) started: String,
    pub(crate) tty: String,
    /// `<pid>-<hash8(started)>` — the pointer filename.
    pub(crate) key: String,
}

/// What last occupied a window. One pointer per window, overwritten on every
/// boot: reading it before writing yours yields your predecessor.
#[derive(Debug, Clone)]
pub(crate) struct Pointer {
    pub(crate) session_id: String,
    /// `process` — bound automatically by the boot hook (the previous occupant
    /// of this terminal). `explicit` — chosen by a human via `session attach`,
    /// which is how you continue a workstream in a window that never ran it.
    pub(crate) kind: String,
    pub(crate) ts: u64,
    pub(crate) repo: String,
    pub(crate) branch: String,
}

impl Pointer {
    pub(crate) fn age_s(&self) -> u64 {
        now_unix().saturating_sub(self.ts)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// FNV-1a. A hash, not a checksum — it only has to distinguish two start times
/// for the same pid, so 32 bits is generous and a dependency would be silly.
fn hash8(s: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

fn ps_field(pid: u32, fmt: &str) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args([format!("-o{fmt}="), format!("-p{pid}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Walk up from this process until a harness process is found.
///
/// Returns `None` when there is no harness ancestor — a human at a plain
/// shell, a CI runner, a daemon. That is a legitimate answer, not an error:
/// there is no window, so there is no lineage, and the caller ranks as before.
pub(crate) fn resolve_window() -> Option<WindowKey> {
    // Escape hatch, and the only way to exercise this end-to-end: name the
    // window explicitly. Also the answer for a harness whose process tree the
    // walk cannot see (a forking supervisor, a container boundary) — export it
    // per terminal and lineage works exactly as if it had been discovered.
    if let Some(k) = env_either("WINDOW_KEY") {
        return Some(WindowKey {
            pid: std::process::id(),
            started: "declared via SVRNMESH_WINDOW_KEY".to_string(),
            tty: env_either("WINDOW_TTY").unwrap_or_else(|| "?".into()),
            key: k.trim().replace(['/', '\\', '.'], "_"),
        });
    }
    let wanted = harness_comms();
    let mut pid = std::process::id();
    for _ in 0..MAX_HOPS {
        let comm = ps_field(pid, "comm").unwrap_or_default();
        let base = comm.rsplit('/').next().unwrap_or(&comm).to_string();
        if wanted.iter().any(|w| w == &base) {
            let started = ps_field(pid, "lstart").unwrap_or_else(|| "unknown".to_string());
            let tty = ps_field(pid, "tty").unwrap_or_else(|| "?".to_string());
            let key = format!("{pid}-{}", hash8(&started));
            return Some(WindowKey {
                pid,
                started,
                tty,
                key,
            });
        }
        let parent: u32 = ps_field(pid, "ppid")?.trim().parse().ok()?;
        if parent <= 1 || parent == pid {
            break;
        }
        pid = parent;
    }
    None
}

pub(crate) fn lineage_root() -> Option<PathBuf> {
    if let Some(d) = env_either("LINEAGE_DIR") {
        return Some(PathBuf::from(d));
    }
    dirs::home_dir().map(|h| h.join(".sovereign").join("lineage"))
}

fn pointer_path(root: &Path, key: &str) -> PathBuf {
    root.join(format!("{key}.json"))
}

pub(crate) fn read_pointer(win: &WindowKey) -> Option<Pointer> {
    let root = lineage_root()?;
    let text = std::fs::read_to_string(pointer_path(&root, &win.key)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    if session_id.is_empty() {
        return None;
    }
    Some(Pointer {
        session_id,
        kind: v
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("process")
            .to_string(),
        ts: v.get("ts").and_then(serde_json::Value::as_u64).unwrap_or(0),
        repo: v
            .get("repo")
            .and_then(|k| k.as_str())
            .unwrap_or_default()
            .to_string(),
        branch: v
            .get("branch")
            .and_then(|k| k.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// Bind this window to `session_id`. Returns an error string rather than
/// panicking or exiting — a lineage write must never be able to break a boot.
pub(crate) fn write_pointer(
    win: &WindowKey,
    session_id: &str,
    kind: &str,
    repo: &str,
    branch: &str,
) -> Result<(), String> {
    let root = lineage_root().ok_or_else(|| "cannot resolve home directory".to_string())?;
    std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let doc = serde_json::json!({
        "schema": "window-lineage/v1",
        "key": win.key,
        "pid": win.pid,
        "started": win.started,
        "tty": win.tty,
        "session_id": session_id,
        "kind": kind,
        "ts": now_unix(),
        "repo": repo,
        "branch": branch,
    });
    let path = pointer_path(&root, &win.key);
    std::fs::write(&path, serde_json::to_string(&doc).unwrap_or_default())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    prune(&root);
    Ok(())
}

pub(crate) fn clear_pointer(win: &WindowKey) -> Result<bool, String> {
    let root = lineage_root().ok_or_else(|| "cannot resolve home directory".to_string())?;
    let path = pointer_path(&root, &win.key);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Drop pointers nobody can ever match again. Best-effort: a failed prune is
/// invisible clutter, never a failed bind.
fn prune(root: &Path) {
    let cutoff = std::time::Duration::from_secs(POINTER_MAX_AGE_DAYS * 86_400);
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let stale = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > cutoff);
        if stale {
            let _ = std::fs::remove_file(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash8_is_stable_and_start_time_sensitive() {
        // The whole safety argument for pid reuse rests on this: same pid,
        // different start time => different key => a stale pointer can never
        // be handed to an unrelated new process.
        assert_eq!(hash8("Mon Jul 27 21:20:09 2026"), hash8("Mon Jul 27 21:20:09 2026"));
        assert_ne!(
            hash8("Mon Jul 27 21:20:09 2026"),
            hash8("Mon Jul 27 21:20:10 2026")
        );
        assert_eq!(hash8("x").len(), 8);
    }

    #[test]
    fn harness_comms_is_overridable() {
        // Other harnesses exist; the walk should not hard-code one binary name.
        // (Serialized implicitly: this is the only test touching the var.)
        std::env::set_var("SOVEREIGN_HARNESS_COMM", "claude, codex ");
        std::env::remove_var("SVRNMESH_HARNESS_COMM");
        assert_eq!(harness_comms(), vec!["claude".to_string(), "codex".to_string()]);
        std::env::remove_var("SOVEREIGN_HARNESS_COMM");
        assert_eq!(harness_comms(), vec!["claude".to_string()]);
    }

    #[test]
    fn pointer_round_trips_through_the_filesystem() {
        let tmp = std::env::temp_dir().join(format!("sov-lineage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let win = WindowKey {
            pid: 4242,
            started: "Mon Jul 27 21:20:09 2026".into(),
            tty: "pts/1".into(),
            key: "4242-deadbeef".into(),
        };
        let doc = serde_json::json!({
            "session_id": "abc-123", "kind": "explicit", "ts": 17,
            "repo": "commonwealth-ai", "branch": "main",
        });
        std::fs::write(
            pointer_path(&tmp, &win.key),
            serde_json::to_string(&doc).unwrap(),
        )
        .unwrap();
        let text = std::fs::read_to_string(pointer_path(&tmp, &win.key)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["session_id"], "abc-123");
        assert_eq!(v["kind"], "explicit");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_window_degrades_to_none_without_a_harness_ancestor() {
        // `cargo test` has no `claude` ancestor, so this must be None and must
        // not panic — that is the fallback path every non-harness caller takes.
        std::env::set_var("SOVEREIGN_HARNESS_COMM", "definitely-not-a-real-process");
        std::env::remove_var("SVRNMESH_HARNESS_COMM");
        std::env::remove_var("SVRNMESH_WINDOW_KEY");
        std::env::remove_var("SOVEREIGN_WINDOW_KEY");
        let got = resolve_window();
        std::env::remove_var("SOVEREIGN_HARNESS_COMM");
        assert!(got.is_none(), "expected no window, got {got:?}");
    }
}
