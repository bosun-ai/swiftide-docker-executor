use std::{
    ops::Deref,
    path::Path,
    sync::{Arc, OnceLock, Weak},
};

use dirs::{home_dir, runtime_dir};
use tokio::sync::Mutex;

use crate::ClientError;

// We use `Weak` in order not to prevent `Drop` of being called.
// Instead, we re-create the client if it was dropped and asked one more time.
// This way we provide on `Drop` guarantees and avoid unnecessary instantiation at the same time.
// <3 testcontainers for figuring this out
static DOCKER_CLIENT: OnceLock<Mutex<Weak<Client>>> = OnceLock::new();
const DEFAULT_DOCKER_SOCKET: &str = "/var/run/docker.sock";

#[derive(Debug)]
pub struct Client {
    bollard_client: bollard::Docker,
    pub socket_path: String,
}

impl Client {
    fn new() -> Result<Self, ClientError> {
        let socket_path = get_socket_path();
        let bollard_client =
            bollard::Docker::connect_with_socket(&socket_path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(ClientError::Init)?;

        Ok(Self {
            bollard_client,
            socket_path,
        })
    }
    /// Returns a client instance, reusing already created or initializing a new one.
    pub(crate) async fn lazy_client() -> Result<Arc<Client>, ClientError> {
        let mut guard = DOCKER_CLIENT
            .get_or_init(|| Mutex::new(Weak::new()))
            .lock()
            .await;
        let maybe_client = guard.upgrade();

        if let Some(client) = maybe_client {
            Ok(client)
        } else {
            let client = Arc::new(Client::new()?);
            *guard = Arc::downgrade(&client);

            Ok(client)
        }
    }
}

impl Deref for Client {
    type Target = bollard::Docker;

    fn deref(&self) -> &Self::Target {
        &self.bollard_client
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
