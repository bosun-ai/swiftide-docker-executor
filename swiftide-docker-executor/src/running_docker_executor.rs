use anyhow::Context as _;
use async_trait::async_trait;
use bollard::{
    query_parameters::{InspectContainerOptions, KillContainerOptions, RemoveContainerOptions},
    secret::{ContainerState, ContainerStateStatusEnum},
};
use codegen::shell_executor_client::ShellExecutorClient;
use futures_util::Stream;
use std::{
    collections::HashMap,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
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
    pub container_port: String,
    pub container_ip: IpAddr,
    dropped: bool,
    retain_on_drop: bool,

    // Default environment configuration for the executor
    pub(crate) env_clear: bool,
    pub(crate) remove_env: Vec<String>,
    pub(crate) env: HashMap<String, String>,
    pub(crate) default_timeout: Option<Duration>,
    pub(crate) workdir: PathBuf,

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
        let workdir = self.resolve_workdir(cmd);
        let timeout = self.resolve_timeout(cmd);

        match cmd {
            Command::Shell { command, .. } => self.exec_shell(command, &workdir, timeout).await,
            Command::ReadFile { path, .. } => self.exec_read_file(&workdir, path, timeout).await,
            Command::WriteFile { path, content, .. } => {
                self.exec_write_file(&workdir, path, content, timeout).await
            }
            _ => unimplemented!(),
        }
    }

    async fn stream_files(
        &self,
        path: &Path,
        extensions: Option<Vec<String>>,
    ) -> anyhow::Result<swiftide_core::indexing::IndexingStream<String>> {
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
            .create_container_config(&image_name, user, &docker)
            .await;

        // Start container
        tracing::info!("Starting container with image: {image_name} and uuid: {container_uuid}");
        let container_starter = ContainerStarter::new(docker.clone());
        let (container_id, container_ip, container_port) = container_starter
            .start_container(&image_name, &container_uuid, container_config)
            .await?;

        // Remove the temporary dockerfile from the container

        let executor = RunningDockerExecutor {
            container_id,
            docker,
            container_port,
            container_ip,
            env_clear: builder.env_clear,
            remove_env: builder.remove_env.clone(),
            env: builder.env.clone(),
            dropped: false,
            retain_on_drop: builder.retain_on_drop,
            cancel_token: Arc::new(CancellationToken::new()),
            default_timeout: builder.default_timeout,
            workdir: builder.workdir.clone(),
        };

        if let Some(tmp_dockerfile_name) = tmp_dockerfile_name {
            let mut removal_targets = vec![tmp_dockerfile_name.clone()];

            if executor.workdir.is_absolute() {
                removal_targets.push(
                    executor
                        .workdir
                        .join(&tmp_dockerfile_name)
                        .display()
                        .to_string(),
                );
            }

            let default_workdir = Path::new("/app");
            if executor.workdir != default_workdir {
                removal_targets.push(
                    default_workdir
                        .join(&tmp_dockerfile_name)
                        .display()
                        .to_string(),
                );
            }

            removal_targets.sort();
            removal_targets.dedup();

            let removal_args = removal_targets
                .iter()
                .map(|target| format!("{target:?}"))
                .collect::<Vec<_>>()
                .join(" ");

            let removal_cmd = format!("rm -f -- {removal_args}");

            executor
                .exec_shell(&removal_cmd, Path::new("/"), executor.default_timeout)
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

    fn resolve_workdir(&self, cmd: &Command) -> PathBuf {
        match cmd.current_dir_path() {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => self.workdir.join(path),
            None => self.workdir.clone(),
        }
    }

    fn resolve_timeout(&self, cmd: &Command) -> Option<Duration> {
        cmd.timeout_duration().copied().or(self.default_timeout)
    }

    async fn exec_shell(
        &self,
        cmd: &str,
        workdir: &Path,
        timeout: Option<Duration>,
    ) -> Result<CommandOutput, CommandError> {
        let mut client = ShellExecutorClient::connect(format!(
            "http://{}:{}",
            self.container_ip, self.container_port
        ))
        .await
        .map_err(anyhow::Error::from)?;

        let timeout_ms = timeout.map(duration_to_millis);
        tracing::debug!(?timeout_ms, "sending shell request with timeout");

        let request = tonic::Request::new(codegen::ShellRequest {
            command: cmd.to_string(),
            env_clear: self.env_clear,
            env_remove: self.remove_env.clone(),
            envs: self.env.clone(),
            timeout_ms,
            cwd: Some(workdir.display().to_string()),
        });

        let response = match client.exec_shell(request).await {
            Ok(resp) => resp.into_inner(),
            Err(status) => {
                if status.code() == tonic::Code::DeadlineExceeded {
                    if let Some(limit) = timeout {
                        let message = status.message().to_string();
                        let output = if message.is_empty() {
                            CommandOutput::empty()
                        } else {
                            CommandOutput::new(message)
                        };

                        return Err(CommandError::TimedOut {
                            timeout: limit,
                            output,
                        });
                    }

                    return Err(CommandError::ExecutorError(status.into()));
                }

                return Err(CommandError::ExecutorError(status.into()));
            }
        };

        let codegen::ShellResponse {
            stdout,
            stderr,
            exit_code,
        } = response;

        let stdout = stdout.trim().to_string();
        let stderr = stderr.trim().to_string();
        let merged = merge_stream_output(&stdout, &stderr);

        if exit_code == 0 {
            Ok(CommandOutput::new(merged))
        } else {
            Err(CommandError::NonZeroExit(CommandOutput::new(merged)))
        }
    }

    #[tracing::instrument(skip(self))]
    async fn exec_read_file(
        &self,
        workdir: &Path,
        path: &Path,
        timeout: Option<Duration>,
    ) -> Result<CommandOutput, CommandError> {
        let cmd = format!("cat {}", path.display());
        self.exec_shell(&cmd, workdir, timeout).await
    }

    #[tracing::instrument(skip(self, content))]
    async fn exec_write_file(
        &self,
        workdir: &Path,
        path: &Path,
        content: &str,
        timeout: Option<Duration>,
    ) -> Result<CommandOutput, CommandError> {
        let cmd = indoc::formatdoc! {
            r#"
            cat << 'EOFKWAAK' > {path}
            {content}
            EOFKWAAK"#,
            path = path.display(),
            content = content.trim_end()

        };

        let write_file_result = self.exec_shell(&cmd, workdir, timeout).await;

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
            let _ = self.exec_shell(&mkdircmd, workdir, timeout).await?;

            return self.exec_shell(&cmd, workdir, timeout).await;
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

fn merge_stream_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

fn duration_to_millis(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    if millis > u64::MAX as u128 {
        u64::MAX
    } else {
        millis as u64
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
