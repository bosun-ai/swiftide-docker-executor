use executor::{MyShellExecutor, codegen::shell_executor_server::ShellExecutorServer};
use tokio::signal::unix::{SignalKind, signal};
use tonic::transport::Server;

mod executor;
#[cfg(feature = "file-loader")]
mod loader;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_target(false)
        .init();
    let addr = "0.0.0.0:50051".parse()?;

    let version = env!("CARGO_PKG_VERSION");
    tracing::warn!("ShellExecutor {version} gRPC server listening on {}", addr);

    let mut builder = Server::builder().add_service(ShellExecutorServer::new(MyShellExecutor));

    #[cfg(feature = "file-loader")]
    {
        use loader::MyLoaderExecutor;
        use loader::codegen::loader_server::LoaderServer;

        tracing::warn!("FileLoader gRPC server listening on {}", addr);
        builder = builder.add_service(LoaderServer::new(MyLoaderExecutor));
    }

    builder.serve_with_shutdown(addr, sigterm()).await?;

    Ok(())
}

async fn sigterm() {
    let _ = signal(SignalKind::terminate())
        .expect("failed to create a new SIGINT signal handler for gRPC")
        .recv()
        .await;

    tracing::warn!("Received SIGTERM, shutting down gracefully...");
}
