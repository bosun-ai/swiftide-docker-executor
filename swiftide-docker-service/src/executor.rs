use std::process::Stdio;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::task::JoinSet;
use tonic::{Request, Response, Status};

// The module `shell` is created by Tonic automatically because your
// package in shell.proto is named `shell`. The name "shell" below must
// match `package shell;` from shell.proto.
pub mod codegen {
    tonic::include_proto!("shell");
}

use codegen::shell_executor_server::ShellExecutor;
use codegen::{ShellRequest, ShellResponse};

#[derive(Debug, Default)]
pub struct MyShellExecutor;

#[tonic::async_trait]
impl ShellExecutor for MyShellExecutor {
    #[tracing::instrument(skip_all)]
    async fn exec_shell(
        &self,
        request: Request<ShellRequest>,
    ) -> Result<Response<ShellResponse>, Status> {
        let ShellRequest {
            command,
            env_clear,
            env_remove,
            envs,
        } = request.into_inner();

        tracing::info!(command, "Received command");

        let lines: Vec<&str> = command.lines().collect();
        let mut child = if let Some(first_line) = lines.first()
            && first_line.starts_with("#!")
        {
            let interpreter = first_line.trim_start_matches("#!/usr/bin/env ").trim();
            tracing::info!(interpreter, "detected shebang; running as script");

            let mut cmd = Command::new(interpreter);

            if env_clear {
                tracing::info!("clearing environment variables");
                cmd.env_clear();
            }

            for var in env_remove {
                tracing::info!(var, "clearing environment variable");
                cmd.env_remove(var);
            }

            for (key, value) in envs {
                tracing::info!(key, "setting environment variable");
                cmd.env(key, value);
            }

            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;

            if let Some(mut stdin) = child.stdin.take() {
                let body = lines[1..].join("\n");
                stdin.write_all(body.as_bytes()).await?;
            }

            child
        } else {
            tracing::info!("no shebang detected; running as command");

            let mut cmd = Command::new("sh");

            if env_clear {
                tracing::info!("clearing environment variables");
                cmd.env_clear();
            }

            for var in env_remove {
                tracing::info!(var, "clearing environment variable");
                cmd.env_remove(var);
            }

            for (key, value) in envs {
                tracing::info!(key, "setting environment variable");
                cmd.env(key, value);
            }
            // Treat as shell command
            cmd.arg("-c")
                .arg(&command)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    tracing::error!(error = ?e, "Failed to start command");
                    Status::internal(format!("Failed to start command: {e:?}"))
                })?
        };
        // Run the command in a shell

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
            Status::internal(format!("Failed to wait for command: {e:?}"))
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
