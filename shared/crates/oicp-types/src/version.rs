// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OICP specification version implemented by this crate.

/// OICP specification version implemented by this module.
pub const OICP_VERSION: &str = "0.4.0";

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_constant_matches_spec() {
        assert_eq!(OICP_VERSION, "0.4.0");
    }
}
