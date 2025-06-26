use anyhow::Context as _;
use async_trait::async_trait;
use bollard::{
    container::LogOutput,
    query_parameters::{InspectContainerOptions, RemoveContainerOptions},
    secret::{ContainerState, ContainerStateStatusEnum},
};
use codegen::shell_executor_client::ShellExecutorClient;
use std::{path::Path, sync::Arc};
pub use swiftide_core::ToolExecutor;
use swiftide_core::{prelude::StreamExt as _, Command, CommandError, CommandOutput};
use uuid::Uuid;

use crate::{
    client::Client, container_configurator::ContainerConfigurator,
    container_starter::ContainerStarter, dockerfile_manager::DockerfileManager,
    image_builder::ImageBuilder, ContextBuilder, ContextError, DockerExecutorError,
};

pub mod codegen {
    tonic::include_proto!("shell");
}

#[derive(Clone, Debug)]
pub struct RunningDockerExecutor {
    pub container_id: String,
    pub(crate) docker: Arc<Client>,
    pub host_port: String,
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
        dockerfile: Option<&Path>,
        image_name: &str,
        user: Option<&str>,
    ) -> Result<RunningDockerExecutor, DockerExecutorError> {
        let docker = Client::lazy_client().await?;

        let mut image_name = image_name.to_string();

        // Any temporary dockrerfile created during the build process
        let mut tmp_dockerfile_name = None;

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

    async fn exec_shell(&self, cmd: &str) -> Result<CommandOutput, CommandError> {
        let mut client =
            ShellExecutorClient::connect(format!("http://127.0.0.1:{}", self.host_port))
                .await
                .map_err(anyhow::Error::from)?;

        let request = tonic::Request::new(codegen::ShellRequest {
            command: cmd.to_string(),
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
        if let Err(CommandError::NonZeroExit(write_file)) = &write_file_result {
            if [
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
