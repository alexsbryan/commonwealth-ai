//! "Is there a config, and does it load?" as one answer for every client that
//! dials from `[daemon]`. A sibling of `setup_config.rs`, which is at its
//! arch-gate ceiling.

use std::path::Path;

use super::SetupConfig;

impl SetupConfig {
    /// Load from the canonical path when a file is there. `Ok(None)` is a first
    /// run (no file: the caller's defaults are the answer); a file that exists
    /// and does not load is `Err` naming the path, never a default: a sandbox
    /// config that failed validation once dialled the operator's node
    /// (phase-b-67, principle 6).
    pub fn load_present() -> Result<Option<Self>, String> {
        Self::migrate_legacy_if_needed();
        Self::load_present_from(&Self::default_path())
    }

    /// [`Self::load_present`] over a given path.
    pub fn load_present_from(path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() {
            tracing::debug!(config = %path.display(), "setup config absent: defaults");
            return Ok(None);
        }
        Self::load_from(path).map(Some).map_err(|e| {
            tracing::debug!(config = %path.display(), error = %e, "setup config present, does not load");
            format!("{} does not load: {e}", path.display())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_present_missing_file_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let got = SetupConfig::load_present_from(&dir.path().join("config.toml"));
        assert!(matches!(got, Ok(None)), "{got:?}");
    }

    /// The incident's sandbox config: it parses, then `validate_class` refuses
    /// it. The refusal names the path; it is not a default port.
    #[test]
    fn load_present_bad_file_refuses_naming_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        for body in ["[daemon]\nclient_port = 19751\n", "[daemon\nclient_port = "] {
            std::fs::write(&path, body).expect("write");
            let err = SetupConfig::load_present_from(&path).expect_err(body);
            assert!(err.contains(&path.display().to_string()), "{err}");
            assert!(err.contains("does not load"), "{err}");
        }
    }

    #[test]
    fn load_present_good_file_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[daemon]\nclient_port = 19751\n[models]\n").expect("write");
        let cfg = SetupConfig::load_present_from(&path)
            .expect("loads")
            .expect("present");
        assert_eq!(cfg.daemon.client_port, 19751);
    }
}
