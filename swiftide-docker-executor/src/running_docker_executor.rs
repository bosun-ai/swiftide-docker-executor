use anyhow::Context as _;
use async_trait::async_trait;
use bollard::{
    container::{
        Config, CreateContainerOptions, LogOutput, LogsOptions, RemoveContainerOptions,
        StartContainerOptions, UploadToContainerOptions,
    },
    exec::{CreateExecOptions, StartExecResults},
    image::{BuildImageOptions, CommitContainerOptions},
    secret::{ContainerState, ContainerStateStatusEnum, PortBinding},
    Docker,
};
use shell::shell_executor_client::ShellExecutorClient;
use std::{collections::HashMap, path::Path, sync::Arc};
pub use swiftide_core::ToolExecutor;
use swiftide_core::{
    prelude::{StreamExt as _, TryStreamExt as _},
    Command, CommandError, CommandOutput,
};
use tracing::info;
use uuid::Uuid;

use crate::{client::Client, ContextBuilder, DockerExecutorError, ServerAssets};

pub mod shell {
    tonic::include_proto!("shell");
}

#[derive(Clone, Debug)]
pub struct RunningDockerExecutor {
    pub container_id: String,
    pub(crate) docker: Arc<Client>,
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
        let docker = Client::lazy_client().await?;

        tracing::warn!(
            "Creating archive for context from {}",
            context_path.display()
        );
        let context = ContextBuilder::from_path(context_path)?.build_tar().await?;

        let tag = container_uuid
            .to_string()
            .split_once('-')
            .map(|(tag, _)| tag)
            .unwrap_or("latest")
            .to_string();

        let image_name_with_tag = format!("{image_name}:{tag}");
        let build_options = BuildImageOptions {
            t: image_name_with_tag.as_str(),
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

        let socket_path = &docker.socket_path;
        let internal_port = "50051/tcp";
        // TODO: Port should be random
        let port_bindings = HashMap::from([(
            internal_port.to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("50051".to_string()),
            }]),
        )]);
        let config = Config {
            image: Some(image_name_with_tag.as_str()),
            cmd: Some(vec!["/bin/sh"]),
            tty: Some(true),
            entrypoint: Some(vec![""]),
            host_config: Some(bollard::models::HostConfig {
                auto_remove: Some(true),
                binds: Some(vec![format!("{socket_path}:/var/run/docker.sock")]),
                port_bindings: Some(port_bindings),
                ..Default::default()
            }),
            ..Default::default()
        };

        let container_name = format!("{image_name}-{container_uuid}");
        let create_options = CreateContainerOptions {
            name: container_name.as_str(),
            ..Default::default()
        };

        tracing::warn!("Creating container from image {image_name}");
        let container_id = docker
            .create_container(Some(create_options), config.clone())
            .await?
            .id;

        tracing::warn!("Starting temporary container {container_id}");
        docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await?;

        tracing::warn!("Uploading embedded server to container {container_id}");
        upload_embedded_server(&docker, &container_id).await?;

        // Now commit the result to the same image name
        let commit_options = CommitContainerOptions {
            container: container_id.as_str(),
            repo: image_name,
            tag: &tag,
            ..Default::default()
        };

        // fml bollard
        let empty = HashMap::<(), ()>::new();
        let mut exposed_ports = HashMap::new();
        exposed_ports.insert("50051/tcp", empty);

        let config = Config {
            cmd: Some(vec!["server"]),
            exposed_ports: Some(exposed_ports),
            // attach_stdout: Some(true),
            // attach_stderr: Some(true),
            ..config
        };
        tracing::warn!("Committing container {container_id} to image {image_name}");
        let commit_result = docker
            .commit_container(commit_options, config.clone())
            .await?;

        tracing::warn!("Committed container {commit_result:?}");

        tracing::warn!("Stopping temporary container {container_id}");
        // stop the container
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

        // create a new container from the committed image
        let container_id = docker
            .create_container(
                Some(CreateContainerOptions {
                    name: container_name.as_str(),
                    ..Default::default()
                }),
                config.clone(),
            )
            .await?
            .id;

        tracing::warn!("Starting actual container {container_id}");
        // start the committed container
        docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await?;

        let mut count = 0;
        while let Some(log) = docker
            .logs(
                &container_id,
                Some(LogsOptions::<&str> {
                    stdout: true,
                    stderr: true,
                    ..Default::default()
                }),
            )
            .next()
            .await
        {
            count += 1;
            dbg!(log);
            if count > 10 {
                break;
            }
        }

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
        let mut client = ShellExecutorClient::connect("http://127.0.0.1:50051")
            .await
            .map_err(anyhow::Error::from)?;

        let request = tonic::Request::new(shell::ShellRequest {
            command: cmd.to_string(),
        });

        let response = client
            .exec_shell(request)
            .await
            .map_err(anyhow::Error::from)?;

        let shell::ShellResponse {
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

pub async fn upload_embedded_server(
    docker: &Docker,
    container_id: &str,
) -> Result<(), DockerExecutorError> {
    let file_data = ServerAssets::get_swiftide_docker_service();

    // Create an in-memory tar archive with one file: "server" (executable)
    let mut tar_buffer = Vec::new();
    let mut builder = tokio_tar::Builder::new(tar_buffer);
    {
        let mut header = tokio_tar::Header::new_gnu();
        header.set_size(file_data.data.len() as u64);
        header.set_mode(0o755); // Make it executable
        header.set_cksum();
        builder
            .append_data(&mut header, "server", file_data.data.as_ref())
            .await?;
        builder.finish().await?;
    }

    // Upload to /usr/local/bin or somewhere in the container
    let options = Some(UploadToContainerOptions {
        path: "/usr/local/bin",
        ..Default::default()
    });

    let bytes = builder.into_inner().await?.into();
    docker
        .upload_to_container(container_id, options, bytes)
        .await?;

    Ok(())
}
