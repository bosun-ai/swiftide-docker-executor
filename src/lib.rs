//! A library for executing swiftide agent tools in a docker container
//!
//!
//! # Example
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let executor = DockerExecutor::default()
//!     .with_context_path(".")
//!     .with_image_name("test")
//!     .with_dockerfile("Dockerfile.overwritten")
//!     .start().await.unwrap();
//!
//! let context = DefaultContext::from_executor(executor);
//! let agent = Agent::builder().context(context);
//! # }
//! ```
mod docker_tool_executor;
mod errors;

pub use docker_tool_executor::*;
pub use errors::*;
