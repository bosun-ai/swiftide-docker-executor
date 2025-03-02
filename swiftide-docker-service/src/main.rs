use grpc_service::{shell::shell_executor_server::ShellExecutorServer, MyShellExecutor};
use tonic::transport::Server;

mod grpc_service;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_target(false)
        .init();
    let addr = "0.0.0.0:50051".parse()?;
    let shell_executor = MyShellExecutor;

    tracing::warn!("ShellExecutor gRPC server listening on {}", addr);

    Server::builder()
        .add_service(ShellExecutorServer::new(shell_executor))
        .serve(addr)
        .await?;

    Ok(())
}
