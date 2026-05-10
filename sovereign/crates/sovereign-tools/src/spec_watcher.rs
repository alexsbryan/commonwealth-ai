//! Filesystem watcher for spec-presence signals.
//!
//! Phase 5b of the CLI refactor. Pairs with the spec-presence
//! cache in [`crate::mcp_surface`] and the MCP
//! `notifications/tools/list_changed` notification surface in
//! `sovereign-mesh::mcp_router`.
//!
//! ## Why
//!
//! The spec-presence cache in `mcp_surface` has a 1-second TTL.
//! Without an FS watcher, an interactive flow like "user creates
//! `.sovereign/features/foo/spec.md`, switches to opencode, asks
//! about the feature" can hit a stale negative cache entry and see
//! the agent advertise the un-gated tool list for up to a second
//! after the spec lands. That's brief but visible — and worse, the
//! MCP server has no way to tell a connected client "your cached
//! `tools/list` is stale, refetch."
//!
//! The watcher closes both gaps:
//!
//! 1. On any spec-relevant FS event, [`SpecWatcher`] calls
//!    [`crate::mcp_surface::invalidate_spec_cache`] for the watched
//!    root, so the next `tools/list` re-stats the disk.
//! 2. The same event fires the caller-supplied `on_change` closure,
//!    which in production wires into an [`McpNotifier`][^notif]
//!    broadcast that pushes
//!    `{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}`
//!    over each subscribed SSE client.
//!
//! [^notif]: `sovereign_mesh::mcp_router::McpNotifier`.
//!
//! ## What we watch
//!
//! Recursive notify watcher rooted at the project directory. Two
//! event-path patterns trigger:
//!
//! - `<root>/ARCHITECTURE.md` (top-level only)
//! - `<root>/.sovereign/features/*/spec.md` (one feature deep)
//!
//! Anything else — `target/`, `.git/`, source files, sibling docs —
//! is dropped at the watcher seam. Notify's HARD_EXCLUDE-equivalent
//! treatment of high-traffic dirs isn't strictly necessary because
//! we filter by suffix-and-prefix, but the recursive watch can
//! still be expensive on very large monorepos. A future iteration
//! can swap to per-directory watches if profiling demands it; for
//! now the simple recursive watch keeps the wiring minimal.
//!
//! ## Lifetime / drop semantics
//!
//! [`SpecWatcher::start`] returns a guard whose `Drop` releases the
//! underlying `notify::RecommendedWatcher` and aborts the dispatch
//! task. The caller (typically `sovereign serve`) holds the guard
//! for the lifetime of the server. Dropping it stops both the FS
//! watcher and the dispatch task — no leaks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::mcp_surface::invalidate_spec_cache;

/// Spec-presence FS watcher. See module-level docs for the why and
/// what.
///
/// Constructed via [`SpecWatcher::start`]; dropped on shutdown.
/// Each `SpecWatcher` watches exactly one root.
pub struct SpecWatcher {
    /// Held alive so the underlying notify backend stays
    /// registered. Drop releases the FD/handle.
    _watcher: RecommendedWatcher,
    /// Held alive so the dispatch task keeps running. Aborted on
    /// drop to release the `mpsc::Receiver` and stop forwarding.
    dispatch: tokio::task::JoinHandle<()>,
    /// Surfaced for tests / introspection. The root we watch.
    root: PathBuf,
}

impl SpecWatcher {
    /// Start a recursive notify watcher rooted at `root` and
    /// dispatch spec-relevant events through `on_change`. The
    /// watcher invalidates the spec-presence cache for `root`
    /// before calling `on_change`, so the closure can rely on the
    /// next [`crate::mcp_surface::render_tools_list_gated`] call
    /// returning a fresh answer.
    ///
    /// `on_change` is invoked from a tokio task (one per
    /// SpecWatcher) so it must be `Send + Sync`. Keep the closure
    /// cheap — it runs on every spec event, including the storm of
    /// Modify events that some editors emit on save.
    ///
    /// Returns a guard whose `Drop` shuts everything down. Pin it
    /// with a `let _watcher = SpecWatcher::start(...)?;` binding
    /// for the server lifetime.
    pub fn start<F>(root: &Path, on_change: F) -> notify::Result<Self>
    where
        F: Fn() + Send + Sync + 'static,
    {
        // The caller passes whatever path they have (typically
        // `repo_root` from cmd_serve, or a tempdir in tests). We
        // need TWO views of it:
        //
        //   - `cache_root` (the original): the spec-presence cache
        //     in `mcp_surface` is keyed on the exact PathBuf the
        //     caller stat()'d. We must invalidate against THAT key,
        //     not a normalised one, otherwise the next
        //     `render_tools_list_gated` call would still hit a
        //     stale entry under the original key.
        //
        //   - `canonical_root` (after canonicalize): on macOS,
        //     FSEvents delivers events with `/private/var/folders/...`
        //     paths while the caller may pass `/var/folders/...`
        //     (the system symlink). Without canonicalising the
        //     prefix used in `path_is_spec_signal`, every event
        //     would silently fail the strip_prefix check.
        let cache_root: PathBuf = root.to_path_buf();
        let canonical_root: PathBuf = std::fs::canonicalize(root)
            .unwrap_or_else(|_| cache_root.clone());
        let on_change = Arc::new(on_change);

        // mpsc bridges from the synchronous notify callback (which
        // runs on a notify worker thread) into the async dispatch
        // task. Bounded with a small buffer; if the dispatch task
        // is slow, we drop events — that's safe because every
        // event triggers the same idempotent cache flush + notify.
        let (tx, mut rx) = mpsc::channel::<Event>(64);

        let watch_root = canonical_root.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            let Ok(event) = res else {
                return;
            };
            if !is_spec_event(&watch_root, &event) {
                return;
            }
            // Fire-and-forget: full channel means a previous event
            // is still being processed; the cache flush + notify
            // it triggers is sufficient for our event too.
            let _ = tx.try_send(event);
        })?;

        watcher.watch(&canonical_root, RecursiveMode::Recursive)?;

        // Async dispatch loop: each event flushes the cache and
        // calls on_change. Coalescing-by-drop in the channel above
        // keeps this from running 100x for one editor save burst.
        let cache_root_for_task = cache_root.clone();
        let canonical_root_for_task = canonical_root.clone();
        let dispatch = tokio::spawn(async move {
            while let Some(_event) = rx.recv().await {
                // Drop both cache keys — they may differ on macOS
                // (caller passes /var/.../foo, FSEvents delivers
                // under /private/var/.../foo). We don't know which
                // one downstream `render_tools_list_gated` will use,
                // so flush both.
                invalidate_spec_cache(&cache_root_for_task);
                if cache_root_for_task != canonical_root_for_task {
                    invalidate_spec_cache(&canonical_root_for_task);
                }
                (on_change)();
            }
        });

        tracing::info!(
            cache_root = %cache_root.display(),
            canonical_root = %canonical_root.display(),
            "spec_watcher: started"
        );
        Ok(Self {
            _watcher: watcher,
            dispatch,
            root: cache_root,
        })
    }

    /// The root this watcher is attached to. Useful for tests and
    /// for log lines.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for SpecWatcher {
    fn drop(&mut self) {
        // Abort the dispatch task. The watcher field's Drop
        // releases the FS handle; together they fully tear down.
        self.dispatch.abort();
        tracing::debug!(
            root = %self.root.display(),
            "spec_watcher: stopped"
        );
    }
}

/// Returns true iff `event` touches a spec-presence signal under
/// `root`:
///
/// - `<root>/ARCHITECTURE.md`, OR
/// - `<root>/.sovereign/features/*/spec.md`.
///
/// Only Create/Modify/Remove events count — Access events from a
/// chatty editor or `cat` shouldn't kick the cache. The path check
/// is the load-bearing filter; everything else (suffix, depth,
/// prefix) keeps unrelated FS noise out of the dispatch loop.
fn is_spec_event(root: &Path, event: &Event) -> bool {
    if !matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return false;
    }
    event.paths.iter().any(|p| path_is_spec_signal(root, p))
}

/// Pure-function path predicate (no FS access) so it's cheap to
/// call from the notify callback. Tests this directly.
///
/// `path` may be absolute or rooted at `root`; we strip the prefix
/// and inspect the relative shape:
///
/// - `ARCHITECTURE.md` (one component) → spec signal.
/// - `.sovereign/features/<id>/spec.md` (four components, last is
///   `spec.md`) → spec signal.
/// - Anything else → not a spec signal.
fn path_is_spec_signal(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let comps: Vec<_> = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    match comps.as_slice() {
        ["ARCHITECTURE.md"] => true,
        [".sovereign", "features", _id, "spec.md"] => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// `path_is_spec_signal` accepts the two canonical signals and
    /// rejects look-alikes (sibling files, deeper paths, the bare
    /// features dir).
    #[test]
    fn path_predicate_matches_two_canonical_signals_only() {
        let root = Path::new("/tmp/foo");

        // Architecture marker.
        assert!(path_is_spec_signal(
            root,
            &root.join("ARCHITECTURE.md"),
        ));
        // Feature spec.
        assert!(path_is_spec_signal(
            root,
            &root.join(".sovereign/features/foo/spec.md"),
        ));

        // Wrong case / sibling files in the same feature dir.
        assert!(!path_is_spec_signal(
            root,
            &root.join(".sovereign/features/foo/brief.md"),
        ));
        assert!(!path_is_spec_signal(
            root,
            &root.join("Architecture.md"),
        ));

        // Deeper path under feature dir doesn't match — only the
        // immediate spec.md does.
        assert!(!path_is_spec_signal(
            root,
            &root.join(".sovereign/features/foo/sub/spec.md"),
        ));

        // README at top level — not a spec signal.
        assert!(!path_is_spec_signal(root, &root.join("README.md")));

        // Path outside the root.
        assert!(!path_is_spec_signal(
            root,
            Path::new("/tmp/bar/ARCHITECTURE.md"),
        ));
    }

    /// End-to-end: the watcher fires on a spec.md write, invalidates
    /// the cache, and calls the on_change closure exactly once
    /// (or more — coalescing is allowed; "≥1" is the contract).
    #[tokio::test]
    async fn watcher_fires_on_spec_md_create() {
        let dir = tempfile::tempdir().unwrap();
        // Drop just our own tempdir's cache entry so we read the
        // empty state fresh. NOT `invalidate_all_spec_caches` —
        // that would wipe entries other tests in this binary may
        // be relying on under cargo's parallel runner.
        crate::mcp_surface::invalidate_spec_cache(dir.path());
        assert!(!crate::mcp_surface::spec_present_in_dir(dir.path()));

        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let _watcher = SpecWatcher::start(dir.path(), move || {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        })
        .expect("spec watcher start");

        // Brief beat so notify finishes registering — on macOS the
        // FSEvents backend takes a few ms to subscribe.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Trigger: write a spec.md.
        let foo = dir.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        // On Linux, notify's recursive mode adds a watch on each
        // newly-discovered subdirectory lazily — the worker thread
        // sees the directory-create events and registers an inner
        // watch *after* the fact. If we write spec.md immediately, the
        // file-create event can fire before the inner watch is in
        // place and we miss it. Production callers (Claude Code,
        // `git checkout`) create the dir long before the file, so this
        // race is test-specific. A brief settle between mkdir and write
        // mirrors the real-world ordering.
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();

        // Poll until the callback fires (or timeout). Notify on
        // macOS is FSEvents-based and can take 100-500ms to deliver.
        // CI under load: keep the deadline generous.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while counter.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            counter.load(Ordering::Relaxed) >= 1,
            "on_change should have fired at least once"
        );
    }

    /// The watcher does NOT fire on noise: a sibling `.md` file in
    /// the feature dir, a top-level `README.md`, etc. Verifies the
    /// path predicate is the actual gate (not just a wishful comment).
    #[tokio::test]
    async fn watcher_ignores_unrelated_writes() {
        let dir = tempfile::tempdir().unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = Arc::clone(&counter);
        let _watcher = SpecWatcher::start(dir.path(), move || {
            counter_clone.fetch_add(1, Ordering::Relaxed);
        })
        .expect("spec watcher start");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Write a noise file: top-level README.md.
        std::fs::write(dir.path().join("README.md"), b"hi").unwrap();
        // And a sibling under a feature dir but not the spec.md.
        let foo = dir.path().join(".sovereign").join("features").join("foo");
        std::fs::create_dir_all(&foo).unwrap();
        std::fs::write(foo.join("brief.md"), b"hi").unwrap();

        // Wait long enough for notify to deliver if it were going
        // to. On macOS FSEvents this is ~500ms typical.
        tokio::time::sleep(Duration::from_millis(800)).await;

        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "noise events must not trigger the spec watcher"
        );
    }

    /// `path_is_spec_signal` is robust to a path that's *exactly*
    /// the root — it must be a no-op (no events under the root
    /// itself match either canonical signal).
    #[test]
    fn path_predicate_is_false_for_the_root_itself() {
        let root = Path::new("/tmp/foo");
        assert!(!path_is_spec_signal(root, root));
    }
}
