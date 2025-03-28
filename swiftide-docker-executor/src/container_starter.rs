use std::{sync::Arc, time::Duration};

use bollard::container::{Config, CreateContainerOptions, LogsOptions, StartContainerOptions};
use swiftide_core::prelude::StreamExt as _;
use uuid::Uuid;

use crate::{client::Client, ContainerStartError};

pub struct ContainerStarter {
    docker: Arc<Client>,
}

impl ContainerStarter {
    pub fn new(docker: Arc<Client>) -> Self {
        Self { docker }
    }

    pub async fn start_container(
        &self,
        image_name: &str,
        container_uuid: &Uuid,
        config: Config<String>,
    ) -> Result<(String, String), ContainerStartError> {
        let container_name = format!("{image_name}-{container_uuid}");
        let create_options = CreateContainerOptions {
            name: container_name.as_str(),
            ..Default::default()
        };

        let container_id = self
            .docker
            .create_container(Some(create_options), config)
            .await
            .map_err(ContainerStartError::Creation)?
            .id;

        self.docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(ContainerStartError::Start)?;

        self.wait_for_logs(&container_id).await?;
        let host_port = self.get_host_port(&container_id).await?;

        Ok((container_id, host_port))
    }

    async fn wait_for_logs(&self, container_id: &str) -> Result<(), ContainerStartError> {
        // We want to give docker some time to start
        // we wait for the 'listening on' message from the grpc client, otherwise we wait and
        // forward logs, up to 10s
        let mut count = 0;
        tokio::time::sleep(Duration::from_millis(100)).await;
        while let Some(log) = self
            .docker
            .logs(
                container_id,
                Some(LogsOptions::<&str> {
                    stdout: true,
                    stderr: true,
                    ..Default::default()
                }),
            )
            .next()
            .await
        {
            if count > 100 {
                tracing::warn!("Waited 10 seconds for container to start; assuming it did");
                break;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;

            let Ok(log) = log
                .as_ref()
                .map_err(|e| ContainerStartError::Logs(e.to_string()))
            else {
                tracing::warn!("Failed to get logs: {:?}", log);
                count += 1;
                continue;
            };
            let log = log.to_string();

            tracing::debug!("Container: {}", &log);

            if log.contains("listening on") {
                tracing::info!("Container started");
                break;
            }

            count += 1;
        }
        Ok(())
    }

    async fn get_host_port(&self, container_id: &str) -> Result<String, ContainerStartError> {
        let container_info = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| ContainerStartError::PortMapping(e.to_string()))?;

        container_info
            .network_settings
            .and_then(|network_settings| network_settings.ports)
            .and_then(|ports| {
                ports.get("50051/tcp").and_then(|maybe_bindings| {
                    maybe_bindings.as_ref().and_then(|bindings| {
                        bindings
                            .first()
                            .and_then(|binding| binding.host_port.clone())
                    })
                })
            })
            .ok_or_else(|| ContainerStartError::PortMapping("Failed to get container port".into()))
    }
}
