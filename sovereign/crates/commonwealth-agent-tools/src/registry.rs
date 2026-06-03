//! Open registry mapping primitive ids → executor invocations. Used
//! by agent loops (the native runner specifically) to dispatch the
//! model's tool calls without a giant match.
//!
//! Per ARCH §4 ("Registry pattern for pluggable dispatch"): the
//! shape allows future operator-supplied primitives to register
//! without modifying this crate. v1 ships with all five canonical
//! primitives pre-registered.

use std::collections::HashMap;

use crate::executor::{execute, ExecCtx};
use crate::primitive::{Primitive, PrimitiveKind};
use crate::result::{ToolError, ToolResult};

/// Registry of canonical primitives. Stores the recognized
/// `PrimitiveKind` set; dispatch goes through
/// `executor::execute` since the executor module already owns the
/// closed match. The registry is the set of ALLOWED primitives the
/// model can invoke, plus per-id descriptors.
#[derive(Debug, Clone)]
pub struct Registry {
    allowed: HashMap<String, PrimitiveKind>,
}

impl Registry {
    /// Empty registry. Use `with_canonical_primitives()` to seed
    /// the v1 set, or `register()` to add bespoke ones.
    pub fn empty() -> Self {
        Self {
            allowed: HashMap::new(),
        }
    }

    /// Registry pre-seeded with every canonical primitive. This is
    /// what the native runner uses.
    pub fn with_canonical_primitives() -> Self {
        let mut r = Self::empty();
        for k in PrimitiveKind::all() {
            r.register(k.id(), *k);
        }
        r
    }

    /// Register a primitive id → kind mapping.
    pub fn register(&mut self, id: &str, kind: PrimitiveKind) {
        self.allowed.insert(id.to_string(), kind);
    }

    /// True iff the registry recognizes this primitive id. Used by
    /// adapters to gate translation.
    pub fn allows(&self, id: &str) -> bool {
        self.allowed.contains_key(id)
    }

    /// Iterate over all registered primitive ids.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.allowed.keys().map(|s| s.as_str())
    }

    /// Resolve a primitive id back to its kind.
    pub fn kind_of(&self, id: &str) -> Option<PrimitiveKind> {
        self.allowed.get(id).copied()
    }

    /// Convenience: dispatch a parsed `Primitive`. Errors with
    /// `InvalidArguments` if the registry doesn't recognize it.
    pub async fn dispatch(&self, ctx: &ExecCtx, prim: &Primitive) -> Result<ToolResult, ToolError> {
        let id = prim.kind().id();
        if !self.allows(id) {
            return Err(ToolError::InvalidArguments {
                primitive: id,
                reason: "primitive not registered in this registry".into(),
            });
        }
        execute(ctx, prim).await
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_canonical_primitives()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_canonical_primitives_seeds_all_kinds() {
        let r = Registry::with_canonical_primitives();
        for kind in PrimitiveKind::all() {
            assert!(r.allows(kind.id()));
            assert_eq!(r.kind_of(kind.id()), Some(*kind));
        }
        assert_eq!(r.ids().count(), PrimitiveKind::all().len());
    }

    #[test]
    fn empty_registry_allows_nothing() {
        let r = Registry::empty();
        for kind in PrimitiveKind::all() {
            assert!(!r.allows(kind.id()));
        }
    }
}
