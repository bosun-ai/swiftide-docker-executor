use executor::{MyShellExecutor, codegen::shell_executor_server::ShellExecutorServer};
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

    tracing::warn!("ShellExecutor gRPC server listening on {}", addr);

    let mut builder = Server::builder().add_service(ShellExecutorServer::new(MyShellExecutor));

    #[cfg(feature = "file-loader")]
    {
        use loader::MyLoaderExecutor;
        use loader::codegen::loader_server::LoaderServer;

        tracing::warn!("FileLoader gRPC server listening on {}", addr);
        builder = builder.add_service(LoaderServer::new(MyLoaderExecutor));
    }

    builder.serve(addr).await?;

    Ok(())
}
