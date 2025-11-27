use std::collections::HashMap;

use bollard::{
    models::ContainerCreateBody,
    query_parameters::InspectContainerOptions,
    secret::{EndpointSettings, NetworkingConfig, PortBinding},
};

use crate::client::Client;

pub struct ContainerConfigurator {
    socket_path: Option<String>,
}

impl ContainerConfigurator {
    pub fn new(socket_path: Option<String>) -> Self {
        Self { socket_path }
    }

    pub async fn create_container_config(
        &self,
        image_name: &str,
        user: Option<&str>,
        docker: &Client,
    ) -> ContainerCreateBody {
        let internal_port = "50051/tcp";
        let port_bindings = HashMap::from([(
            internal_port.to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("".to_string()),
            }]),
        )]);

        let empty = HashMap::<(), ()>::new();
        let mut exposed_ports = HashMap::new();
        exposed_ports.insert("50051/tcp".to_string(), empty);

        // Check if we're running inside a container and try to get the network name
        let mut network_config = None;
        let maybe_network = self.maybe_docker_network(docker).await;
        if let Some(network) = maybe_network.as_deref() {
            tracing::info!(?network, "using discovered docker network");
            let mut endpoints = HashMap::<String, EndpointSettings>::new();
            endpoints.insert(network.to_string(), Default::default());
            network_config = Some(NetworkingConfig {
                endpoints_config: Some(endpoints),
            });
        }

        ContainerCreateBody {
            image: Some(image_name.to_string()),
            cmd: Some(vec!["swiftide-docker-service".to_string()]),
            tty: Some(true),
            user: user.map(|u| u.to_string()),
            exposed_ports: Some(exposed_ports),
            networking_config: network_config,
            host_config: Some(bollard::models::HostConfig {
                auto_remove: Some(true),
                binds: if let Some(socket_path) = self.socket_path.as_ref() {
                    Some(vec![format!("{}:/var/run/docker.sock", socket_path)])
                } else {
                    None
                },
                port_bindings: Some(port_bindings),
                network_mode: maybe_network.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    async fn maybe_docker_network(&self, docker: &Client) -> Option<String> {
        // Check if we're running inside a container and try to get the network name
        let self_id = std::env::var("HOSTNAME").unwrap_or_else(|_| "".into());
        let me = docker
            .inspect_container(&self_id, None::<InspectContainerOptions>)
            .await
            .ok()?;
        let nets = me.network_settings.and_then(|ns| ns.networks)?;

        // Pick the first network not bridge
        nets.keys()
            .find(|k| *k != "bridge")
            .or_else(|| nets.keys().next())
            .cloned()
    }
}
