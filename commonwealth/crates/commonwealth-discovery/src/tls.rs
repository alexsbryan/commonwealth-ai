// SPDX-License-Identifier: AGPL-3.0-or-later
use rcgen::{CertificateParams, DnType, KeyPair};
use serde::{Deserialize, Serialize};

use commonwealth_core::ids::NodeId;
use commonwealth_core::{Error, Result};

/// A node's TLS identity — certificate + private key pair.
/// Generated during mesh init or join handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// PEM-encoded certificate.
    pub certificate_pem: String,
    /// PEM-encoded private key.
    pub private_key_pem: String,
    pub node_id: NodeId,
}

/// Generate a self-signed TLS certificate for a node.
/// The certificate includes the node ID in the common name for identification.
pub fn generate_node_identity(node_id: NodeId, node_name: &str) -> Result<NodeIdentity> {
    let key_pair =
        KeyPair::generate().map_err(|e| Error::Tls(format!("failed to generate key pair: {e}")))?;

    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, format!("commonwealth-{node_id}"));
    params.distinguished_name.push(
        DnType::OrganizationName,
        format!("Commonwealth Node: {node_name}"),
    );

    // Self-signed, valid for 10 years.
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2034, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| Error::Tls(format!("failed to self-sign certificate: {e}")))?;

    Ok(NodeIdentity {
        certificate_pem: cert.pem(),
        private_key_pem: key_pair.serialize_pem(),
        node_id,
    })
}

/// Collection of trusted peer certificates for mTLS verification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    /// Map from node ID to their PEM-encoded certificate.
    pub trusted_certs: Vec<TrustedCert>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedCert {
    pub node_id: NodeId,
    pub certificate_pem: String,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a trusted peer certificate (received during join handshake).
    pub fn add_cert(&mut self, node_id: NodeId, certificate_pem: String) {
        // Replace if already exists.
        self.trusted_certs.retain(|c| c.node_id != node_id);
        self.trusted_certs.push(TrustedCert {
            node_id,
            certificate_pem,
        });
    }

    /// Get a trusted peer's certificate.
    pub fn get_cert(&self, node_id: NodeId) -> Option<&str> {
        self.trusted_certs
            .iter()
            .find(|c| c.node_id == node_id)
            .map(|c| c.certificate_pem.as_str())
    }

    /// Remove a peer's certificate (e.g., after revocation).
    pub fn remove_cert(&mut self, node_id: NodeId) {
        self.trusted_certs.retain(|c| c.node_id != node_id);
    }

    pub fn len(&self) -> usize {
        self.trusted_certs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trusted_certs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_identity_produces_valid_pem() {
        let node_id = NodeId::from_u128(1);
        let identity = generate_node_identity(node_id, "Test Node").unwrap();

        assert!(identity.certificate_pem.contains("BEGIN CERTIFICATE"));
        assert!(identity.private_key_pem.contains("BEGIN PRIVATE KEY"));
        assert_eq!(identity.node_id, node_id);
    }

    #[test]
    fn identity_serde_roundtrip() {
        let node_id = NodeId::from_u128(1);
        let identity = generate_node_identity(node_id, "Test Node").unwrap();

        let json = serde_json::to_string(&identity).unwrap();
        let back: NodeIdentity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.node_id, node_id);
        assert!(back.certificate_pem.contains("BEGIN CERTIFICATE"));
    }

    #[test]
    fn trust_store_operations() {
        let mut store = TrustStore::new();
        assert!(store.is_empty());

        let id1 = NodeId::from_u128(1);
        let id2 = NodeId::from_u128(2);

        store.add_cert(id1, "cert-1".into());
        store.add_cert(id2, "cert-2".into());
        assert_eq!(store.len(), 2);
        assert_eq!(store.get_cert(id1), Some("cert-1"));

        // Replace existing.
        store.add_cert(id1, "cert-1-updated".into());
        assert_eq!(store.len(), 2);
        assert_eq!(store.get_cert(id1), Some("cert-1-updated"));

        // Remove.
        store.remove_cert(id1);
        assert_eq!(store.len(), 1);
        assert!(store.get_cert(id1).is_none());
    }

    #[test]
    fn two_nodes_get_distinct_certs() {
        let id1 = NodeId::from_u128(1);
        let id2 = NodeId::from_u128(2);
        let cert1 = generate_node_identity(id1, "Node 1").unwrap();
        let cert2 = generate_node_identity(id2, "Node 2").unwrap();
        assert_ne!(cert1.certificate_pem, cert2.certificate_pem);
        assert_ne!(cert1.private_key_pem, cert2.private_key_pem);
    }
}
