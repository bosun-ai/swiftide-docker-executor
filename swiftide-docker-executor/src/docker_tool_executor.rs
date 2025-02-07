use std::path::PathBuf;
use uuid::Uuid;

use crate::{DockerExecutorError, RunningDockerExecutor};

/// Build a docker image with bollard and start it up
#[derive(Clone, Debug)]
pub struct DockerExecutor {
    context_path: PathBuf,
    image_name: String,
    dockerfile: PathBuf,
    container_uuid: Uuid,
}

impl Default for DockerExecutor {
    fn default() -> Self {
        Self {
            container_uuid: Uuid::new_v4(),
            context_path: ".".into(),
            image_name: "docker-executor".into(),
            dockerfile: "Dockerfile".into(),
        }
    }
}

impl DockerExecutor {
    /// Set the path to build the context from (default ".")
    pub fn with_context_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.context_path = path.into();

        self
    }

    /// Set the name of the image to build (default "docker-executor")
    pub fn with_image_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.image_name = name.into();

        self
    }

    /// Overwrite the uuid that is added as suffix to the running container
    pub fn with_container_uuid(&mut self, uuid: impl Into<Uuid>) -> &mut Self {
        self.container_uuid = uuid.into();

        self
    }

    /// Overwrite the dockerfile to use (default "Dockerfile")
    pub fn with_dockerfile(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.dockerfile = path.into();
        self
    }

    /// Starts the docker executor
    ///
    /// Note that on dropping the `RunningDockerExecutor`, the container will be stopped
    pub async fn start(self) -> Result<RunningDockerExecutor, DockerExecutorError> {
        RunningDockerExecutor::start(
            self.container_uuid,
            &self.context_path,
            &self.dockerfile,
            &self.image_name,
        )
        .await
    }
}
