use std::collections::HashMap;

use bollard::{models::ContainerCreateBody, secret::PortBinding};

pub struct ContainerConfigurator {
    socket_path: String,
}

impl ContainerConfigurator {
    pub fn new(socket_path: String) -> Self {
        Self { socket_path }
    }

    pub fn create_container_config(
        &self,
        image_name: &str,
        user: Option<&str>,
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

        ContainerCreateBody {
            image: Some(image_name.to_string()),
            cmd: Some(vec!["swiftide-docker-service".to_string()]),
            tty: Some(true),
            entrypoint: Some(vec!["".to_string()]),
            user: user.map(|u| u.to_string()),
            exposed_ports: Some(exposed_ports),
            host_config: Some(bollard::models::HostConfig {
                auto_remove: Some(true),
                binds: Some(vec![format!("{}:/var/run/docker.sock", self.socket_path)]),
                port_bindings: Some(port_bindings),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}
