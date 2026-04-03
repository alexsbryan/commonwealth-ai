use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! define_id {
    ($name:ident, $prefix:expr) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name([u8; 16]);

        impl $name {
            pub fn generate() -> Self {
                let mut bytes = [0u8; 16];
                getrandom::fill(&mut bytes).expect("failed to generate random bytes");
                Self(bytes)
            }

            /// Create an ID from a u128 value. Useful for deterministic test IDs.
            pub fn from_u128(val: u128) -> Self {
                Self(val.to_be_bytes())
            }

            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}-{}", $prefix, hex::encode(&self.0[..8]))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self)
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.0.cmp(&other.0)
            }
        }
    };
}

define_id!(MeshId, "mesh");
define_id!(NodeId, "node");
define_id!(ModelId, "model");
define_id!(ProcessId, "proc");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_display_format() {
        let id = NodeId::from_u128(0x0123456789abcdef_0000000000000000);
        let s = id.to_string();
        assert!(s.starts_with("node-"));
        assert_eq!(s, "node-0123456789abcdef");
    }

    #[test]
    fn id_ordering_is_deterministic() {
        let a = NodeId::from_u128(1);
        let b = NodeId::from_u128(2);
        assert!(a < b);
    }

    #[test]
    fn id_serde_roundtrip() {
        let id = MeshId::generate();
        let json = serde_json::to_string(&id).unwrap();
        let back: MeshId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn distinct_id_types_are_not_interchangeable() {
        // This is a compile-time guarantee, but we verify they're distinct types
        let node = NodeId::from_u128(1);
        let mesh = MeshId::from_u128(1);
        // Same bytes, but different types — this would fail to compile:
        // let _: NodeId = mesh;
        assert_eq!(node.as_bytes(), mesh.as_bytes());
    }
}
