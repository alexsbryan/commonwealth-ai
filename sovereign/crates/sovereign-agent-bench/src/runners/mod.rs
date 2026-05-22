//! Concrete `AgentRunner` implementations + their registry.

pub mod mock;
pub mod native;
pub mod pi;
pub mod registry;
pub mod shared_detectors;

pub use mock::MockAgentRunner;
pub use native::NativeRunner;
pub use pi::PiRunner;
pub use registry::AgentRunnerRegistry;
