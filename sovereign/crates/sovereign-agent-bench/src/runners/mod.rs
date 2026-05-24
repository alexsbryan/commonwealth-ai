//! Concrete `AgentRunner` implementations + their registry.

pub mod bare_metal;
pub mod mock;
pub mod native;
pub mod pi;
pub mod registry;
pub mod search;
pub mod shared;
pub mod shared_detectors;

pub use bare_metal::BareMetalRunner;
pub use mock::MockAgentRunner;
pub use native::NativeRunner;
pub use pi::PiRunner;
pub use registry::AgentRunnerRegistry;
pub use search::SearchRunner;
