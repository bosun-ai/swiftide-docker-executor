use tokio::process::Command;
use tonic::{transport::Server, Request, Response, Status};

// The module `shell` is created by Tonic automatically because your
// package in shell.proto is named `shell`. The name "shell" below must
// match `package shell;` from shell.proto.
pub mod shell {
    tonic::include_proto!("shell");
}

use shell::shell_executor_server::{ShellExecutor, ShellExecutorServer};
use shell::{ShellRequest, ShellResponse};

#[derive(Debug, Default)]
pub struct MyShellExecutor;

#[tonic::async_trait]
impl ShellExecutor for MyShellExecutor {
    async fn exec_shell(
        &self,
        request: Request<ShellRequest>,
    ) -> Result<Response<ShellResponse>, Status> {
        let ShellRequest { command } = request.into_inner();

        // Run the command in a shell
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .await
            .map_err(|e| Status::internal(format!("Failed to run command: {:?}", e)))?;

        // Prepare response
        let response = ShellResponse {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        };

        Ok(Response::new(response))
    }
}
