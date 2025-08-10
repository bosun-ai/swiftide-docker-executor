use anyhow::Context as _;
use async_trait::async_trait;
use bollard::{
    query_parameters::{InspectContainerOptions, KillContainerOptions, RemoveContainerOptions},
    secret::{ContainerState, ContainerStateStatusEnum},
};
use codegen::shell_executor_client::ShellExecutorClient;
use futures_util::Stream;
use std::{collections::HashMap, path::Path, sync::Arc};
pub use swiftide_core::ToolExecutor;
use swiftide_core::{Command, CommandError, CommandOutput, Loader as _, prelude::StreamExt as _};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::{
    ContextBuilder, ContextError, DockerExecutor, DockerExecutorError, client::Client,
    container_configurator::ContainerConfigurator, container_starter::ContainerStarter,
    dockerfile_manager::DockerfileManager, image_builder::ImageBuilder,
};

pub mod codegen {
    tonic::include_proto!("shell");
}
pub use bollard::container::LogOutput;

#[derive(Clone, Debug)]
pub struct RunningDockerExecutor {
    pub container_id: String,
    pub(crate) docker: Arc<Client>,
    pub host_port: String,
    dropped: bool,
    retain_on_drop: bool,

    // Default environment configuration for the executor
    pub(crate) env_clear: bool,
    pub(crate) remove_env: Vec<String>,
    pub(crate) env: HashMap<String, String>,

    /// Cancellation token to stop anything polling the docker api
    cancel_token: Arc<CancellationToken>,
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

    async fn stream_files(
        &self,
        path: &Path,
        extensions: Option<Vec<String>>,
    ) -> anyhow::Result<swiftide_core::indexing::IndexingStream> {
        let extensions = extensions.unwrap_or_default();
        Ok(self.as_file_loader(path, extensions).into_stream())
    }
}

impl RunningDockerExecutor {
    /// Starts a docker container with a given context and image name
    pub async fn start(
        builder: &DockerExecutor,
    ) -> Result<RunningDockerExecutor, DockerExecutorError> {
        let docker = Client::lazy_client().await?;

        // Any temporary dockrerfile created during the build process
        let mut tmp_dockerfile_name = None;
        let mut image_name = builder.image_name.clone();
        let dockerfile = &builder.dockerfile;
        let context_path = &builder.context_path;
        let user = builder.user.as_deref();
        let container_uuid = builder.container_uuid;

        // Only build if a dockerfile is provided
        if let Some(dockerfile) = dockerfile {
            // Prepare dockerfile
            let dockerfile_manager = DockerfileManager::new(context_path);
            let tmp_dockerfile = dockerfile_manager.prepare_dockerfile(dockerfile).await?;

            // Build context
            tracing::warn!(
                "Creating archive for context from {}",
                context_path.display()
            );
            let context = ContextBuilder::from_path(context_path, tmp_dockerfile.path())?
                .build_tar()
                .await?;

            tracing::debug!("Context build with size: {} bytes", context.len());

            let tmp_dockerfile_name_inner = tmp_dockerfile
                .path()
                .file_name()
                .ok_or_else(|| {
                    ContextError::CustomDockerfile("Could not read custom dockerfile".to_string())
                })
                .map(|s| s.to_string_lossy().to_string())?;

            drop(tmp_dockerfile); // Make sure the temporary file is removed right away

            // Build image
            let tag = container_uuid
                .to_string()
                .split_once('-')
                .map(|(tag, _)| tag)
                .unwrap_or("latest")
                .to_string();

            let image_builder = ImageBuilder::new(docker.clone());
            let image_name_with_tag = image_builder
                .build_image(
                    context,
                    tmp_dockerfile_name_inner.as_ref(),
                    &image_name,
                    &tag,
                )
                .await?;

            image_name = image_name_with_tag;
            tmp_dockerfile_name = Some(tmp_dockerfile_name_inner);
        }

        // Configure container
        let container_config = ContainerConfigurator::new(docker.socket_path.clone())
            .create_container_config(&image_name, user);

        // Start container
        tracing::info!("Starting container with image: {image_name} and uuid: {container_uuid}");
        let container_starter = ContainerStarter::new(docker.clone());
        let (container_id, host_port) = container_starter
            .start_container(&image_name, &container_uuid, container_config)
            .await?;

        // Remove the temporary dockerfile from the container

        let executor = RunningDockerExecutor {
            container_id,
            docker,
            host_port,
            env_clear: builder.env_clear,
            remove_env: builder.remove_env.clone(),
            env: builder.env.clone(),
            dropped: false,
            retain_on_drop: builder.retain_on_drop,
            cancel_token: Arc::new(CancellationToken::new()),
        };

        if let Some(tmp_dockerfile_name) = tmp_dockerfile_name {
            executor
                .exec_shell(&format!("rm {}", tmp_dockerfile_name.as_str()))
                .await
                .context("failed to remove temporary dockerfile")
                .map_err(DockerExecutorError::Start)?;
        }

        Ok(executor)
    }

    /// Returns the underlying bollard status of the container
    ///
    /// Useful for checking if the executor is running or not
    pub async fn container_state(&self) -> Result<ContainerState, DockerExecutorError> {
        let container = self
            .docker
            .inspect_container(&self.container_id, None::<InspectContainerOptions>)
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

    /// Returns the logs of the container
    pub async fn logs(&self) -> Result<Vec<String>, DockerExecutorError> {
        let mut logs = Vec::new();
        let mut stream = self.docker.logs(
            &self.container_id,
            Some(bollard::query_parameters::LogsOptions {
                follow: false,
                stdout: true,
                stderr: true,
                tail: "all".to_string(),
                ..Default::default()
            }),
        );

        while let Some(log_result) = stream.next().await {
            match log_result {
                Ok(log_output) => match log_output {
                    LogOutput::Console { message }
                    | LogOutput::StdOut { message }
                    | LogOutput::StdErr { message } => {
                        logs.push(String::from_utf8_lossy(&message).trim().to_string());
                    }
                    _ => {}
                },
                Err(e) => tracing::error!("Error retrieving logs: {e}"), // Error handling
            }
        }

        Ok(logs)
    }

    /// Streams the logs of the container as raw `bollard::container::LogOutput` items.
    pub async fn logs_stream(
        &self,
    ) -> impl Stream<Item = Result<LogOutput, bollard::errors::Error>> {
        let docker = self.docker.clone();
        let container_id = self.container_id.clone();
        let cancel = self.cancel_token.clone();

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        tokio::spawn(async move {
            tokio::select!(
                _ = cancel.cancelled() => {
                    tracing::debug!("Logs stream cancelled");
                },
                _ = async move {
                    let mut stream = docker.logs(
                        &container_id,
                        Some(bollard::query_parameters::LogsOptions {
                            follow: true,
                            stdout: true,
                            stderr: true,
                            tail: "all".to_string(),
                            ..Default::default()
                        }),
                    );
                    tracing::debug!("Starting logs stream for container");
                    while let Some(log_result) = stream.next().await {
                        if let Err(err) = tx.send(log_result)
                        .await {
                            return tracing::error!("Failed to send log item: {}", err);
                        }
                    }
                } => {
                    tracing::debug!("Logs stream ended gracefully");
                },
                else => {
                    tracing::error!("Logs stream ended unexpectedly");
                }
            );

            tracing::debug!("Closing logs stream channel");
        });

        ReceiverStream::new(rx)
    }

    async fn exec_shell(&self, cmd: &str) -> Result<CommandOutput, CommandError> {
        let mut client =
            ShellExecutorClient::connect(format!("http://127.0.0.1:{}", self.host_port))
                .await
                .map_err(anyhow::Error::from)?;

        let request = tonic::Request::new(codegen::ShellRequest {
            command: cmd.to_string(),
            env_clear: self.env_clear,
            env_remove: self.remove_env.clone(),
            envs: self.env.clone(),
        });

        let response = client
            .exec_shell(request)
            .await
            .map_err(anyhow::Error::from)?;

        let codegen::ShellResponse {
            stdout,
            stderr,
            exit_code,
        } = response.into_inner();

        // // Trim both stdout and stderr to remove surrounding whitespace and newlines
        let output = stdout.trim().to_string() + stderr.trim();
        //
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
        if let Err(CommandError::NonZeroExit(write_file)) = &write_file_result
            && [
                "no such file or directory",
                "directory nonexistent",
                "nonexistent directory",
            ]
            .iter()
            .any(|&s| write_file.output.to_lowercase().contains(s))
        {
            let path = path.parent().context("No parent directory")?;
            let mkdircmd = format!("mkdir -p {}", path.display());
            let _ = self.exec_shell(&mkdircmd).await?;

            return self.exec_shell(&cmd).await;
        }

        write_file_result
    }

    /// Stops and removes the container associated with this executor.
    pub async fn shutdown(&self) -> Result<(), DockerExecutorError> {
        // Stop any jobs that might block the docker socket
        self.cancel_token.cancel();

        tracing::warn!(
            "Dropped; stopping and removing container {container_id}",
            container_id = self.container_id
        );

        let docker = self.docker.clone();
        let container_id = self.container_id.clone();

        tracing::debug!(
            "Stopping container {container_id}",
            container_id = container_id
        );
        docker
            .kill_container(
                &container_id,
                Some(KillContainerOptions {
                    signal: "SIGTERM".to_string(),
                }),
            )
            .await?;

        tracing::debug!(
            "Removing container {container_id}",
            container_id = container_id
        );

        docker
            .remove_container(
                &container_id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                }),
            )
            .await?;

        Ok(())
    }
}

impl Drop for RunningDockerExecutor {
    fn drop(&mut self) {
        if self.dropped {
            tracing::debug!(
                "Executor already dropped; skipping stop and remove for container {}",
                self.container_id
            );
            return;
        }
        if self.retain_on_drop {
            tracing::debug!(
                "Retaining container {} on drop; not stopping or removing",
                self.container_id
            );
            return;
        }
        self.dropped = true;
        self.cancel_token.cancel();

        let this = self.clone();
        let container_id = self.container_id.clone();

        tokio::task::spawn_blocking(move || {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move { this.shutdown().await })
        });
        tracing::debug!("Container stopped {container_id}");
    }
}
