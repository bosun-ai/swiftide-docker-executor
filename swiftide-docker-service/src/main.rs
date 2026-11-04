use executor::{MyShellExecutor, codegen::shell_executor_server::ShellExecutorServer};
use tokio::signal::unix::{SignalKind, signal};
use tonic::transport::Server;

mod command_cleanup;
mod executor;
#[cfg(feature = "file-loader")]
mod loader;
mod sandbox;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Before anything else, so that the launcher stays single-threaded.
    let mut args = std::env::args_os().skip(1);
    if args
        .next()
        .is_some_and(|arg| arg == sandbox::LAUNCHER_SUBCOMMAND)
    {
        sandbox::launch(args);
    }

    serve()
}

#[tokio::main]
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_target(false)
        .init();
    // Overridable so tests can run several services side by side.
    let addr = std::env::var("SWIFTIDE_DOCKER_SERVICE_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:50051".to_string())
        .parse()?;

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
