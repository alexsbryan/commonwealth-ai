// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tenant identity: the key a host scopes stored state by.
//!
//! Lives in this leaf rather than in the server that extracts it because
//! `commonwealth-core` and `sovereign-*` cannot see each other in either
//! direction (`quality/ARCH_LAYERS.toml:125-128`), and `oicp-types` is the
//! serde-only crate both already depend on —
//! `sovereign/deploy/mesh/GROUND_TRUTH.md` §"The layer contract that decides
//! where `TenantId` lives".

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The tenant a request acts as — the key a host scopes stored state by.
///
/// # What is valid, and why
///
/// A tenant id is not free text: it is a **key segment**. Hosts `format!` it
/// into scoped conversation ids (`"{tenant}:{conversation_id}"`) and list by
/// the `"{tenant}:"` prefix, and compare it verbatim to a document asset's
/// owner. Every rule below names the call-site hazard that earns it; there is
/// deliberately **no length cap**, because no call site justifies one.
///
/// Checks run in this order, so a value that violates several is reported by
/// the first — the id is a key segment first, so separator violations are
/// ranked ahead of the traversal form:
///
/// 1. The input is trimmed. Empty or whitespace-only →
///    [`Empty`](InvalidTenantId::Empty). An empty tenant collapses the scoped
///    id to `":{conversation_id}"` and the list prefix to `":"`, which stops
///    distinguishing tenants at all.
/// 2. Internal whitespace → [`Whitespace`](InvalidTenantId::Whitespace). The id
///    round-trips through config maps and is compared verbatim; whitespace
///    makes two visually identical ids unequal.
/// 3. `/` or `\` → [`PathSeparator`](InvalidTenantId::PathSeparator). The id is
///    compared against a document asset's owner and concatenated into ids that
///    reach storage keys; a path separator is the escape character there.
/// 4. `:` → [`ScopeSeparator`](InvalidTenantId::ScopeSeparator). `:` is the
///    separator the scoped conversation id is built from, so a tenant
///    containing one is a genuine cross-tenant collision: tenant `a` with
///    conversation `b:c` and tenant `a:b` with conversation `c` both produce
///    `a:b:c`, and the `"a:"` prefix listing returns both.
/// 5. `..` → [`Traversal`](InvalidTenantId::Traversal). Catches the traversal
///    form that carries no separator of its own.
///
/// # Construction
///
/// Use [`TenantId::parse`] for anything that came from configuration or the
/// wire, and [`TenantId::default_tenant`] for the tenant a host assigns when
/// auth is disabled. There is deliberately **no `Default` impl**: a default
/// that materializes on its own is exactly the silent substitution an identity
/// type must not do (ARCH §18.3), so the one valid-by-construction literal is
/// reached by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// The tenant a host assigns when auth is disabled. Valid by
    /// construction — it passes every rule in [`TenantId`]'s doc comment,
    /// which the `tenant_id_default_is_valid_by_construction` test pins so a
    /// later rule cannot make this literal quietly unparseable.
    pub const DEFAULT: &'static str = "default";

    /// The [`DEFAULT`](Self::DEFAULT) tenant, built without going through
    /// fallible parsing.
    pub fn default_tenant() -> Self {
        Self(Self::DEFAULT.to_string())
    }

    /// Parse a raw tenant id — from a config key map, or off the wire.
    ///
    /// Leading and trailing whitespace is trimmed; everything else is
    /// rejected per the rules on [`TenantId`].
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, InvalidTenantId> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Err(InvalidTenantId::Empty);
        }
        if raw.chars().any(|c| c.is_whitespace()) {
            return Err(InvalidTenantId::Whitespace);
        }
        if raw.contains('/') || raw.contains('\\') {
            return Err(InvalidTenantId::PathSeparator);
        }
        if raw.contains(':') {
            return Err(InvalidTenantId::ScopeSeparator);
        }
        if raw.contains("..") {
            return Err(InvalidTenantId::Traversal);
        }
        Ok(Self(raw.to_string()))
    }

    /// The tenant id's wire form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reasons [`TenantId`] construction can fail. One variant per rejection
/// rule; the rule each one enforces is on [`TenantId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidTenantId {
    /// The input was empty or whitespace-only, which collapses every scoped
    /// id and prefix the tenant is built into.
    Empty,
    /// The input contained internal whitespace, so two visually identical
    /// ids would not compare equal.
    Whitespace,
    /// The input contained a path separator (`/` or `\`), the escape
    /// character for the storage keys the id reaches.
    PathSeparator,
    /// The input contained `:`, the separator scoped conversation ids are
    /// built from — a cross-tenant collision.
    ScopeSeparator,
    /// The input contained `..`, the traversal form that carries no
    /// separator of its own.
    Traversal,
}

impl std::fmt::Display for InvalidTenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::Empty => "tenant id is empty",
            Self::Whitespace => "tenant id contains whitespace",
            Self::PathSeparator => "tenant id contains a path separator ('/' or '\\')",
            Self::ScopeSeparator => "tenant id contains ':', the scoped-id separator",
            Self::Traversal => "tenant id contains '..'",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for InvalidTenantId {}

impl Serialize for TenantId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TenantId {
    /// Deserialization goes through [`TenantId::parse`]: a permissive
    /// `Deserialize` would be a hole straight past the validator for every
    /// value that arrives over the wire.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

// -----------------------------------------------------------------
// Tests
// -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_parse_rejects_empty() {
        assert_eq!(TenantId::parse("").unwrap_err(), InvalidTenantId::Empty);
        assert_eq!(TenantId::parse("   ").unwrap_err(), InvalidTenantId::Empty);
    }

    #[test]
    fn tenant_id_parse_rejects_path_traversal() {
        assert_eq!(
            TenantId::parse("../other").unwrap_err(),
            InvalidTenantId::PathSeparator
        );
        assert_eq!(
            TenantId::parse("..").unwrap_err(),
            InvalidTenantId::Traversal
        );
    }

    #[test]
    fn tenant_id_parse_accepts_a_plain_key() {
        let t = TenantId::parse("acme-prod").expect("a plain key is a valid tenant id");
        assert_eq!(t.as_str(), "acme-prod");
        assert_eq!(t.to_string(), "acme-prod");
        // Trimming, not rejection, for the outer edges.
        assert_eq!(
            TenantId::parse("  firm  ").expect("trimmed").as_str(),
            "firm"
        );
    }

    #[test]
    fn tenant_id_parse_rejects_the_scoped_id_separator() {
        // `a:b` + conversation `c` and `a` + conversation `b:c` would both
        // scope to `a:b:c` — see the collision named on `TenantId`.
        assert_eq!(
            TenantId::parse("a:b").unwrap_err(),
            InvalidTenantId::ScopeSeparator
        );
        assert_eq!(
            TenantId::parse("a b").unwrap_err(),
            InvalidTenantId::Whitespace
        );
        assert_eq!(
            TenantId::parse("a\\b").unwrap_err(),
            InvalidTenantId::PathSeparator
        );
    }

    #[test]
    fn tenant_id_default_is_valid_by_construction() {
        // The auth-disabled path builds this without parsing; if the rules
        // ever grew to reject it, that path would be silently wrong.
        assert_eq!(
            TenantId::parse(TenantId::DEFAULT).expect("DEFAULT parses"),
            TenantId::default_tenant()
        );
    }

    #[test]
    fn tenant_id_deserialize_runs_the_validator() {
        assert!(serde_json::from_str::<TenantId>("\"firm\"").is_ok());
        assert!(serde_json::from_str::<TenantId>("\"../other\"").is_err());
    }
}
