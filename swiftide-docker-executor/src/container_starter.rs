use std::net::{IpAddr, ToSocketAddrs};
use std::str::FromStr;
use std::{sync::Arc, time::Duration};

use bollard::{
    query_parameters::{
        CreateContainerOptions, InspectContainerOptions, LogsOptions, StartContainerOptions,
    },
    secret::ContainerCreateBody,
};
use swiftide_core::prelude::StreamExt as _;
use uuid::Uuid;

use crate::{ContainerStartError, client::Client};

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
        config: ContainerCreateBody,
    ) -> Result<(String, IpAddr, String), ContainerStartError> {
        // Strip tag suffix from image name if present
        let image_name = if let Some(index) = image_name.find(':') {
            &image_name[..index]
        } else {
            image_name
        };

        // Replace invalid characters in image name ('/')
        let image_name = image_name.replace('/', "-");

        let container_name = format!("{image_name}-{container_uuid}");
        let create_options = CreateContainerOptions {
            name: Some(container_name),
            ..Default::default()
        };

        let container_id = self
            .docker
            .create_container(Some(create_options), config)
            .await
            .map_err(ContainerStartError::Creation)?
            .id;

        tracing::info!("Created container with ID: {}", &container_id);

        self.docker
            .start_container(&container_id, None::<StartContainerOptions>)
            .await
            .map_err(ContainerStartError::Start)?;

        self.wait_for_logs(&container_id).await?;
        let (ip, port) = self.get_ip_and_port(&container_id).await?;

        Ok((container_id, ip, port))
    }

    async fn wait_for_logs(&self, container_id: &str) -> Result<(), ContainerStartError> {
        // We want to give docker some time to start
        // we wait for the 'listening on' message from the grpc client, otherwise we wait and
        // forward logs, up to 10s
        let mut count = 0;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut stream = self.docker.logs(
            container_id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                follow: true,
                since: 0,
                ..Default::default()
            }),
        );

        while let Some(log) = stream.next().await {
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

    async fn get_ip_and_port(
        &self,
        container_id: &str,
    ) -> Result<(IpAddr, String), ContainerStartError> {
        // If we have a container ip, return inner port and ip
        // If we have a host gateway ip (we i.e. running inside docker compose or docker), return that and the inner port
        // Otherwise return localhost and the mapped port
        if let Some(ip) = self.get_container_ip(container_id).await? {
            return Ok((ip, "50051".into()));
        }

        if self.is_running_in_docker() && let Some(ip) = self.host_gateway_ip() {
            return Ok((ip, "50051".into()));
        }

        let container_port = self.get_container_port(container_id).await?;
        Ok((IpAddr::from_str("127.0.0.1").unwrap(), container_port))
    }

    async fn get_container_ip(
        &self,
        container_id: &str,
    ) -> Result<Option<IpAddr>, ContainerStartError> {
        let container_info = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| ContainerStartError::PortMapping(e.to_string()))?;

        Ok(container_info
            .network_settings
            .and_then(|ns| ns.networks)
            .map(|nets| {
                tracing::debug!(networks = ?nets, "Container networks");
                nets
            })
            .and_then(|nets| nets.into_iter().find(|(k, _)| *k != "bridge"))
            .and_then(|(_, endpoint)| endpoint.ip_address)
            .as_deref()
            .map(IpAddr::from_str)
            .transpose()?)
    }

    async fn get_container_port(&self, container_id: &str) -> Result<String, ContainerStartError> {
        let container_info = self
            .docker
            .inspect_container(container_id, None::<InspectContainerOptions>)
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

    /// Tries to resolve the local docker gateway if it's present
    fn host_gateway_ip(&self) -> Option<IpAddr> {
        ("host.docker.internal", 0)
            .to_socket_addrs()
            .ok()
            .and_then(|v| v.take(1).next().map(|addr| addr.ip()))
    }

    fn is_running_in_docker(&self) -> bool {
        std::fs::metadata("/.dockerenv").is_ok()
    }
}
