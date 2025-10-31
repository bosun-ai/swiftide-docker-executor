use std::{collections::HashMap, path::PathBuf, time::Duration};
use uuid::Uuid;

use crate::{DockerExecutorError, RunningDockerExecutor};

/// Build a docker image with bollard and start it up
#[derive(Clone, Debug)]
pub struct DockerExecutor {
    pub(crate) context_path: PathBuf,
    pub(crate) image_name: String,
    pub(crate) dockerfile: Option<PathBuf>,
    pub(crate) container_uuid: Uuid,
    pub(crate) user: Option<String>,
    pub(crate) env_clear: bool,
    pub(crate) remove_env: Vec<String>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) retain_on_drop: bool,
    pub(crate) default_timeout: Option<Duration>,
}

impl Default for DockerExecutor {
    fn default() -> Self {
        Self {
            container_uuid: Uuid::new_v4(),
            context_path: ".".into(),
            image_name: "docker-executor".into(),
            dockerfile: Some("Dockerfile".into()),
            user: None,
            env: HashMap::new(),
            env_clear: false,
            remove_env: vec![],
            retain_on_drop: false,
            default_timeout: None,
        }
    }
}

impl DockerExecutor {
    /// Set the path to build the context from (default ".")
    pub fn with_context_path(&mut self, path: impl Into<PathBuf>) -> &mut Self {
        self.context_path = path.into();

        self
    }

    /// Instead of killing the container on drop, retain it for inspection. Default is false.
    pub fn retain_on_drop(&mut self, retain: bool) -> &mut Self {
        self.retain_on_drop = retain;

        self
    }

    /// Clear the environment variables before starting the service in the container
    pub fn clear_env(&mut self) -> &mut Self {
        self.env_clear = true;

        self
    }

    /// Remove an environment variable from the service in the container
    pub fn remove_env(&mut self, env: impl Into<String>) -> &mut Self {
        self.remove_env.push(env.into());

        self
    }

    /// Set an environment variable for the service in the container
    pub fn with_env(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.env.insert(key.into(), value.into());

        self
    }

    /// Set multiple environment variables for the service in the container
    pub fn with_envs(&mut self, envs: impl Into<HashMap<String, String>>) -> &mut Self {
        self.env.extend(envs.into());

        self
    }

    /// Use the provided timeout as the default for every command executed against the container.
    pub fn with_default_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.default_timeout = Some(timeout);

        self
    }

    /// Remove any default timeout previously configured on this executor.
    pub fn clear_default_timeout(&mut self) -> &mut Self {
        self.default_timeout = None;

        self
    }

    /// Set the user (or user_id:group_id) to run the container as (default None, which means root)
    pub fn with_user(&mut self, user: impl Into<String>) -> &mut Self {
        self.user = Some(user.into());

        self
    }

    /// Start with an existing image (full tag). Will skip building the image, unless you set a new
    /// Dockerfile. Note that this requires that the image has the service available as a binary.
    pub fn with_existing_image(&mut self, path: impl Into<String>) -> &mut Self {
        self.image_name = path.into();

        // If an existing image is used, we don't need to build it
        self.dockerfile = None;

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
        self.dockerfile = Some(path.into());
        self
    }

    /// Starts the docker executor
    ///
    /// Note that on dropping the `RunningDockerExecutor`, the container will be stopped
    pub async fn start(self) -> Result<RunningDockerExecutor, DockerExecutorError> {
        RunningDockerExecutor::start(&self).await
    }
}
