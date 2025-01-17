use anyhow::Context as _;
use async_trait::async_trait;
use dirs::{home_dir, runtime_dir};
use std::{path::Path, sync::Arc};
pub use swiftide_core::ToolExecutor;
use swiftide_core::{prelude::StreamExt as _, Command, CommandError, CommandOutput};
use tracing::info;
use uuid::Uuid;

const DEFAULT_DOCKER_SOCKET: &str = "/var/run/docker.sock";

use bollard::{
    container::{
        Config, CreateContainerOptions, LogOutput, RemoveContainerOptions, StartContainerOptions,
    },
    exec::{CreateExecOptions, StartExecResults},
    image::BuildImageOptions,
    secret::{ContainerState, ContainerStateStatusEnum},
    Docker, API_DEFAULT_VERSION,
};

use crate::{ContextBuilder, DockerExecutorError};

#[derive(Clone, Debug)]
pub struct RunningDockerExecutor {
    pub container_id: String,
    pub(crate) docker: Docker,
}

impl From<RunningDockerExecutor> for Arc<dyn ToolExecutor> {
    fn from(val: RunningDockerExecutor) -> Self {
        Arc::new(val) as Arc<dyn ToolExecutor>
    }
}

#[async_trait]
impl ToolExecutor for RunningDockerExecutor {
    #[tracing::instrument(skip(self), err)]
    async fn exec_cmd(&self, cmd: &Command) -> Result<CommandOutput, CommandError> {
        match cmd {
            Command::Shell(cmd) => self.exec_shell(cmd).await,
            Command::ReadFile(path) => self.read_file(path).await,
            Command::WriteFile(path, content) => self.write_file(path, content).await,
            _ => unimplemented!(),
        }
    }
}

impl RunningDockerExecutor {
    /// Starts a docker container with a given context and image name
    pub async fn start(
        container_uuid: Uuid,
        context_path: &Path,
        dockerfile: &Path,
        image_name: &str,
    ) -> Result<RunningDockerExecutor, DockerExecutorError> {
        let socket_path = get_socket_path();
        let docker = Docker::connect_with_socket(&socket_path, 120, API_DEFAULT_VERSION)?;

        tracing::warn!(
            "Creating archive for context from {}",
            context_path.display()
        );
        let context = ContextBuilder::from_path(context_path)?.build_tar().await?;

        let image_name = format!("kwaak-{image_name}");
        let build_options = BuildImageOptions {
            t: image_name.as_str(),
            rm: true,
            dockerfile: &dockerfile.to_string_lossy(),
            ..Default::default()
        };

        tracing::warn!("Building docker image with name {image_name}");
        {
            let mut build_stream = docker.build_image(build_options, None, Some(context.into()));

            while let Some(log) = build_stream.next().await {
                match log {
                    Ok(output) => {
                        if let Some(stream) = output.stream {
                            info!("{}", stream);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error building image: {e:#}");
                        return Err(e.into());
                    }
                }
            }
        }

        let config = Config {
            image: Some(image_name.as_str()),
            tty: Some(true),
            cmd: Some(vec!["sleep", "infinity"]),
            entrypoint: Some(vec![""]),
            host_config: Some(bollard::models::HostConfig {
                auto_remove: Some(true),
                binds: Some(vec![format!("{socket_path}:/var/run/docker.sock")]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let container_name = format!("kwaak-{image_name}-{container_uuid}");
        let create_options = CreateContainerOptions {
            name: container_name.as_str(),
            ..Default::default()
        };

        tracing::warn!("Creating container from image {image_name}");
        let container_id = docker
            .create_container(Some(create_options), config)
            .await?
            .id;

        tracing::warn!("Starting container {container_id}");
        docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await?;

        Ok(RunningDockerExecutor {
            container_id,
            docker,
        })
    }

    /// Returns the underlying bollard status of the container
    ///
    /// Useful for checking if the executor is running or not
    pub async fn container_state(&self) -> Result<ContainerState, DockerExecutorError> {
        let container = self
            .docker
            .inspect_container(&self.container_id, None)
            .await?;

        container.state.ok_or_else(|| {
            DockerExecutorError::ContainerStateMissing(self.container_id.to_string())
        })
    }

    /// Check if the executor and its underlying container is running
    ///
    /// Will ignore any errors and assume it is not if there are
    pub async fn is_running(&self) -> bool {
        self.container_state()
            .await
            .map(|state| state.status == Some(ContainerStateStatusEnum::RUNNING))
            .unwrap_or(false)
    }

    async fn exec_shell(&self, cmd: &str) -> Result<CommandOutput, CommandError> {
        let cmd = vec!["sh", "-c", cmd];
        tracing::debug!("Executing command {cmd}", cmd = cmd.join(" "));

        let exec = self
            .docker
            .create_exec(
                &self.container_id,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd),
                    ..Default::default()
                },
            )
            .await
            .context("Failed to create docker exec")?
            .id;

        let mut stdout = String::new();
        let mut stderr = String::new();

        if let StartExecResults::Attached { mut output, .. } = self
            .docker
            .start_exec(&exec, None)
            .await
            .context("Failed to start docker exec")?
        {
            while let Some(Ok(msg)) = output.next().await {
                match msg {
                    LogOutput::StdErr { .. } => stderr.push_str(&msg.to_string()),
                    LogOutput::StdOut { .. } => stdout.push_str(&msg.to_string()),
                    _ => {
                        stderr
                            .push_str("Command appears to wait for input, which is not supported");
                        break;
                    }
                }
            }
        } else {
            todo!();
        }

        let exec_inspect = self
            .docker
            .inspect_exec(&exec)
            .await
            .context("Failed to inspect docker exec result")?;
        let exit_code = exec_inspect.exit_code.unwrap_or(0);

        // Trim both stdout and stderr to remove surrounding whitespace and newlines
        let output = stdout.trim().to_string() + stderr.trim();

        if exit_code == 0 {
            Ok(output.into())
        } else {
            Err(CommandError::NonZeroExit(output.into()))
        }
    }

    #[tracing::instrument(skip(self))]
    async fn read_file(&self, path: &Path) -> Result<CommandOutput, CommandError> {
        self.exec_shell(&format!("cat {}", path.display())).await
    }

    #[tracing::instrument(skip(self, content))]
    async fn write_file(&self, path: &Path, content: &str) -> Result<CommandOutput, CommandError> {
        let cmd = indoc::formatdoc! {r#"
            cat << 'EOFKWAAK' > {path}
            {content}
            EOFKWAAK"#,
            path = path.display(),
            content = content.trim_end()

        };

        let write_file_result = self.exec_shell(&cmd).await;

        // If the directory or file does not exist, create it
        if let Err(CommandError::NonZeroExit(write_file)) = &write_file_result {
            if ["No such file or directory", "Directory nonexistent"]
                .iter()
                .any(|&s| write_file.output.contains(s))
            {
                let path = path.parent().context("No parent directory")?;
                let mkdircmd = format!("mkdir -p {}", path.display());
                let _ = self.exec_shell(&mkdircmd).await?;

                return self.exec_shell(&cmd).await;
            }
        }

        write_file_result
    }
}

impl Drop for RunningDockerExecutor {
    fn drop(&mut self) {
        tracing::warn!(
            "Stopping container {container_id}",
            container_id = self.container_id
        );
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.docker
                    .remove_container(
                        &self.container_id,
                        Some(RemoveContainerOptions {
                            force: true,
                            v: true,
                            ..Default::default()
                        }),
                    )
                    .await
            })
        });

        if let Err(e) = result {
            tracing::warn!(error = %e, "Error stopping container, might not be stopped");
        }
    }
}

/// Lovingly copied from testcontainers-rs
/// Reliably gets the path to the docker socket
fn get_socket_path() -> String {
    validate_path("/var/run/docker.sock".into())
        .or_else(|| {
            runtime_dir()
                .and_then(|dir| validate_path(format!("{}/.docker/run/docker.sock", dir.display())))
        })
        .or_else(|| {
            home_dir()
                .and_then(|dir| validate_path(format!("{}/.docker/run/docker.sock", dir.display())))
        })
        .or_else(|| {
            home_dir().and_then(|dir| {
                validate_path(format!("{}/.docker/desktop/docker.sock", dir.display()))
            })
        })
        .unwrap_or(DEFAULT_DOCKER_SOCKET.into())
}

fn validate_path(path: String) -> Option<String> {
    if Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}
