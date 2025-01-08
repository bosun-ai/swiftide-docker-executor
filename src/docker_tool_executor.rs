use std::path::PathBuf;
use uuid::Uuid;

use crate::{DockerExecutorError, RunningDockerExecutor};

/// Build a docker image with bollard and start it up
#[derive(Clone, Debug)]
pub struct DockerExecutor {
    context_path: PathBuf,
    image_name: String,
    #[allow(dead_code)]
    working_dir: PathBuf,
    dockerfile: PathBuf,
    container_uuid: Uuid,
}

impl Default for DockerExecutor {
    fn default() -> Self {
        Self {
            container_uuid: Uuid::new_v4(),
            context_path: ".".into(),
            image_name: "docker-executor".into(),
            working_dir: ".".into(),
            dockerfile: "Dockerfile".into(),
        }
    }
}

impl DockerExecutor {
    pub fn with_context_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.context_path = path.into();

        self
    }

    pub fn with_image_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.image_name = name.into();

        self
    }

    pub fn with_container_uuid(&mut self, uuid: impl Into<Uuid>) -> &mut Self {
        self.container_uuid = uuid.into();

        self
    }

    pub fn with_dockerfile(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.dockerfile = path.into();
        self
    }

    #[allow(dead_code)]
    pub fn with_working_dir(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.working_dir = path.into();

        self
    }

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

// Iterate over all the files in the context directory and adds it to an in memory
// tar. Respects .gitignore and .dockerignore.
