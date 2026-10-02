// SPDX-License-Identifier: AGPL-3.0-or-later
//! The boot's start step. svrn holds no mesh of its own: cw-rails is the
//! node's one mesh endpoint (pb-mesh-exit-transport), so the daemon starts on
//! every boot and never founds, resumes or joins a mesh itself.

use std::sync::Arc;

use sovereign_core::setup_config::SetupConfig;

/// Where a fleet joiner goes now that the config join is gone.
const JOIN_POINTER: &str = "svrn mesh join <invite>";

/// Start the daemon. `Some(exit_code)` aborts boot.
///
/// A config naming `[discovery] join_key` is refused by name (phase-b-80
/// fork 2): it was the plaintext `relay=` join the flip retires with
/// plaintext meshes (phase-b-36), and a node that booted anyway would sit off
/// the fleet it was configured for, silently.
pub(super) async fn start(
    daemon: &Arc<crate::EmbeddedDaemon>,
    config: &SetupConfig,
) -> Option<i32> {
    if config.discovery.join_key.is_some() {
        tracing::error!(
            target: "mesh",
            "boot refused: [discovery] join_key names a config join, which is retired"
        );
        eprintln!(
            "error: [discovery] join_key is set, and the config join is retired: cw-rails is \
             this node's mesh endpoint and joins only by invite. Remove join_key and \
             seed_addrs from [discovery], then run `{JOIN_POINTER}` with an invite a member \
             prints (`svrn mesh create` on the founder, `svrn mesh rotate` on any member)."
        );
        return Some(1);
    }
    if let Err(e) = daemon.start().await {
        eprintln!("error: the daemon did not start: {e}");
        return Some(1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The PROOF line "a `join_key` config exits at boot naming the verb":
    /// the start step answers an exit code and the daemon never runs.
    #[tokio::test]
    async fn a_config_naming_a_join_key_exits_before_the_daemon_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = SetupConfig::unconfigured();
        config.discovery.join_key = Some("a-retired-config-join".into());
        let daemon = crate::EmbeddedDaemon::new(
            tmp.path().to_path_buf(),
            config.clone(),
            crate::daemon_services::fixtures::headless(),
        );
        assert_eq!(start(&daemon, &config).await, Some(1));
        assert!(
            !daemon.is_running().await,
            "a refused boot started the daemon"
        );
        assert_eq!(JOIN_POINTER, "svrn mesh join <invite>");
    }
}
