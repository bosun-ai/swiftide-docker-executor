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
mod container_configurator;
mod container_starter;
mod context_builder;
mod docker_tool_executor;
mod dockerfile_manager;
mod dockerfile_mangler;
mod errors;
mod image_builder;
mod running_docker_executor;

pub mod file_loader;

#[cfg(test)]
mod tests;

pub use context_builder::*;
pub use docker_tool_executor::*;
pub use errors::*;
pub use running_docker_executor::*;
