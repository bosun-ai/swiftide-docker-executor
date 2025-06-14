use std::process::Stdio;

use tokio::io::AsyncBufReadExt as _;
use tokio::process::Command;
use tokio::task::JoinSet;
use tonic::{Request, Response, Status};

// The module `shell` is created by Tonic automatically because your
// package in shell.proto is named `shell`. The name "shell" below must
// match `package shell;` from shell.proto.
pub mod shell {
    tonic::include_proto!("shell");
}

use shell::shell_executor_server::ShellExecutor;
use shell::{ShellRequest, ShellResponse};

#[derive(Debug, Default)]
pub struct MyShellExecutor;

#[tonic::async_trait]
impl ShellExecutor for MyShellExecutor {
    #[tracing::instrument(skip_all)]
    async fn exec_shell(
        &self,
        request: Request<ShellRequest>,
    ) -> Result<Response<ShellResponse>, Status> {
        let ShellRequest { command } = request.into_inner();

        tracing::info!(command, "Received command");

        // Run the command in a shell
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                tracing::error!(error = ?e, "Failed to start command");
                Status::internal(format!("Failed to start command: {:?}", e))
            })?;

        // NOTE: Feels way overcomplicated just because we want both stderr and stdout
        let mut joinset = JoinSet::new();

        if let Some(stdout) = child.stdout.take() {
            joinset.spawn(async move {
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                let mut out = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!("stdout: {line}");
                    out.push(line);
                }
                out
            });
        } else {
            tracing::warn!("Command has no stdout");
        }

        if let Some(stderr) = child.stderr.take() {
            joinset.spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                let mut out = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!("stderr: {line}");
                    out.push(line);
                }
                out
            });
        } else {
            tracing::warn!("Command has no stderr");
        }

        let outputs = joinset.join_all().await;
        let &[stdout, stderr] = outputs
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>()
            .as_slice()
        else {
            // This should never happen
            tracing::error!("Failed to get outputs");
            return Err(Status::internal("Failed to join stdout and stderr"));
        };

        // outputs stdout and stderr should be empty
        let output = child.wait_with_output().await.map_err(|e| {
            tracing::error!(error = ?e, "Failed to wait for command");
            Status::internal(format!("Failed to wait for command: {:?}", e))
        })?;

        debug_assert!(stdout.is_empty());
        debug_assert!(stderr.is_empty());

        // Prepare response
        let response = ShellResponse {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: stdout.join("\n"),
            stderr: stderr.join("\n"),
        };

        tracing::info!(command, exit_code = response.exit_code, "Command executed");

        Ok(Response::new(response))
    }
}
