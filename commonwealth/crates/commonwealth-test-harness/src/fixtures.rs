use std::collections::HashMap;

use commonwealth_core::capabilities::ComputeType;
use commonwealth_core::ids::ModelId;
use commonwealth_core::model::{ModelArchitecture, ModelInfo};
use commonwealth_core::oicp::{Capability, CapabilityProfile};

use crate::simulated_mesh::SimulatedMesh;
use crate::simulated_node::SimulatedNodeBuilder;

/// Create a test model with the given parameters.
pub fn test_model(id: u128, name: &str, layers: u32, size_gb: u64) -> ModelInfo {
    ModelInfo {
        id: ModelId::from_u128(id),
        name: name.into(),
        repo: format!("test/{name}"),
        file: format!("{name}.gguf"),
        size_bytes: size_gb * 1_073_741_824,
        total_layers: layers,
        architecture: ModelArchitecture::Qwen,
        available_on: HashMap::new(),
        oicp_capabilities: CapabilityProfile::default(),
        quantization: "Q4_K_M".into(),
    }
}

/// Create a coding model with appropriate OICP capabilities.
pub fn coding_model(id: u128) -> ModelInfo {
    let mut caps = CapabilityProfile::default();
    caps.insert(Capability::Code, 4);
    caps.insert(Capability::Instruction, 3);
    caps.insert(Capability::General, 2);

    let mut model = test_model(id, "qwen3-coder-30b", 64, 17);
    model.oicp_capabilities = caps;
    model
}

/// Create a general-purpose model with appropriate OICP capabilities.
pub fn general_model(id: u128) -> ModelInfo {
    let mut caps = CapabilityProfile::default();
    caps.insert(Capability::General, 3);
    caps.insert(Capability::Analysis, 3);
    caps.insert(Capability::Creative, 3);
    caps.insert(Capability::Code, 2);

    let mut model = test_model(id, "qwen3-30b", 64, 17);
    model.oicp_capabilities = caps;
    model
}

/// Build the five-node mesh from the architecture document.
/// Returns the mesh and node indices: (alice, bob, carol, dave, eve).
pub fn architecture_five_node_mesh() -> SimulatedMesh {
    let mut mesh = SimulatedMesh::new("Sunset District Co-op");

    // Alice: Strix Halo, 32 GB shared
    mesh.add_node(
        SimulatedNodeBuilder::new(1, "Alice's Desktop")
            .gpu("Strix Halo", 32, ComputeType::Vulkan)
            .ram_gb(32),
    );

    // Bob: RTX 4090, 24 GB
    mesh.add_node(
        SimulatedNodeBuilder::new(2, "Bob's Build")
            .gpu("RTX 4090", 24, ComputeType::Cuda)
            .ram_gb(64),
    );

    // Carol: M3 Ultra, 192 GB unified
    mesh.add_node(
        SimulatedNodeBuilder::new(3, "Carol's Mac")
            .gpu("Apple M3 Ultra", 144, ComputeType::Metal)
            .ram_gb(192)
            .storage_gb(2000, 1500),
    );

    // Dave: 2× RTX 3090, 48 GB total
    mesh.add_node(
        SimulatedNodeBuilder::new(4, "Dave's Rig")
            .gpu("RTX 3090", 24, ComputeType::Cuda)
            .gpu("RTX 3090", 24, ComputeType::Cuda)
            .ram_gb(64),
    );

    // Eve: MacBook Air, 16 GB shared, integrated
    mesh.add_node(
        SimulatedNodeBuilder::new(5, "Eve's MacBook Air")
            .gpu("Apple M3", 12, ComputeType::Metal)
            .ram_gb(16)
            .storage_gb(256, 100),
    );

    mesh.set_lan_latency(1.0);
    mesh
}

/// Helper to make an HTTP GET request to a node.
pub async fn http_get(addr: std::net::SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.writable().await.unwrap();
    stream.try_write(request.as_bytes()).unwrap();

    let mut response = Vec::new();
    loop {
        stream.readable().await.unwrap();
        let mut buf = [0u8; 4096];
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }

    let response_str = String::from_utf8_lossy(&response);

    // Parse status code.
    let status_code = response_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    // Parse body.
    let body = response_str
        .find("\r\n\r\n")
        .map(|pos| &response_str[pos + 4..])
        .unwrap_or("");

    let json = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    (status_code, json)
}

/// Helper to make an HTTP POST request with JSON body.
pub async fn http_post(
    addr: std::net::SocketAddr,
    path: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let body_str = serde_json::to_string(body).unwrap();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body_str}",
        body_str.len()
    );
    stream.writable().await.unwrap();
    stream.try_write(request.as_bytes()).unwrap();

    let mut response = Vec::new();
    loop {
        stream.readable().await.unwrap();
        let mut buf = [0u8; 4096];
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(_) => break,
        }
    }

    let response_str = String::from_utf8_lossy(&response);
    let status_code = response_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    let body = response_str
        .find("\r\n\r\n")
        .map(|pos| &response_str[pos + 4..])
        .unwrap_or("");

    let json = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    (status_code, json)
}
