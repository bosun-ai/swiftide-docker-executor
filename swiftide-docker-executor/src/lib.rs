//! A library for executing swiftide agent tools in a docker container
//!
//!
//! # Example
//!
//! ```no_run
//! # use swiftide_agents::Agent;
//! # use swiftide_docker_executor::DockerExecutor;
//! # use swiftide_agents::DefaultContext;
//! # use swiftide_core::ToolExecutor;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let executor = DockerExecutor::default()
//!     .with_context_path(".")
//!     .with_image_name("test")
//!     .with_dockerfile("Dockerfile.overwritten")
//!     .to_owned()
//!     .start().await.unwrap();
//!
//! let context = DefaultContext::from_executor(executor);
//! let agent = Agent::builder().context(context);
//! # Ok(())
//! # }
//! ```
mod client;
mod context_builder;
mod docker_tool_executor;
mod dockerfile_mangler;
mod errors;
mod running_docker_executor;

#[cfg(test)]
mod tests;

pub use context_builder::*;
pub use docker_tool_executor::*;
pub use errors::*;
pub use running_docker_executor::*;

use rust_embed::{EmbeddedFile, RustEmbed};

#[derive(RustEmbed)]
#[folder = "src/resources"]
pub struct ServerAssets;

impl ServerAssets {
    pub fn get_swiftide_docker_service() -> EmbeddedFile {
        Self::get("swiftide-docker-service").expect("Failed to get embedded server")
    }
}
