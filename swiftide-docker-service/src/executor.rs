use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time;
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
            timeout_ms,
            cwd,
        } = request.into_inner();

        let timeout = timeout_ms.map(Duration::from_millis);
        tracing::debug!(?timeout, "resolved timeout for shell request");

        tracing::info!(command, "Received command");

        let workdir = cwd.unwrap_or_else(|| ".".to_string());
        let workdir_path = Path::new(&workdir);

        let has_bash = Path::new("/bin/bash").exists();

        if is_background(&command) {
            tracing::info!("Running command in background");
            let mut cmd = Command::new(if has_bash { "/bin/bash" } else { "sh" });
            if has_bash {
                cmd.arg("--login");
            }

            apply_env_settings(&mut cmd, env_clear, env_remove, envs);

            // Don't capture stdout or stderr, and don't wait for child process.
            cmd.arg("-c")
                .arg(command)
                .current_dir(workdir_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());

            // Spawn and detach
            match cmd.spawn() {
                Ok(_child) => {
                    // Optionally: don't keep handle, just return success immediately
                    return Ok(Response::new(ShellResponse {
                        exit_code: 0,
                        stdout: String::from("Background command started"),
                        stderr: String::new(),
                    }));
                }
                Err(e) => {
                    // Handle error spawning command
                    return Err(Status::internal(format!(
                        "Failed to start background command: {e:?}"
                    )));
                }
            }
        }

        let lines: Vec<&str> = command.lines().collect();
        let mut child = if let Some(first_line) = lines.first()
            && first_line.starts_with("#!")
        {
            let shebang = first_line.trim_start_matches("#!").trim();
            let mut parts = shebang.split_whitespace();
            let interpreter = parts.next().unwrap_or("").to_string();
            let args: Vec<String> = parts.map(|s| s.to_string()).collect();

            tracing::info!(interpreter, args = ?args, "detected shebang; running as script");

            // Run interpreter from within a login shell so profile files are honored,
            // while still executing the original shebang interpreter (python, sh, etc.).
            let mut cmd = Command::new(if has_bash { "/bin/bash" } else { "sh" });
            if has_bash {
                cmd.arg("--login");
                cmd.arg("-c");
                cmd.arg("exec \"$@\"");
                cmd.arg("bash"); // $0 for -c
            } else {
                // Many /bin/sh implementations accept -l for login shells.
                cmd.arg("-l");
                cmd.arg("-c");
                cmd.arg("exec \"$@\"");
                cmd.arg("sh");
            }

            cmd.arg(&interpreter);
            cmd.args(&args);
            apply_env_settings(&mut cmd, env_clear, env_remove, envs);

            let mut child = cmd
                .current_dir(workdir_path)
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

            let mut cmd = Command::new(if has_bash { "/bin/bash" } else { "sh" });

            apply_env_settings(&mut cmd, env_clear, env_remove, envs);

            if has_bash {
                cmd.arg("--login");
            }
            cmd.arg("-c")
                .arg(&command)
                .current_dir(workdir_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    tracing::error!(error = ?e, "Failed to start command");
                    Status::internal(format!("Failed to start command: {e:?}"))
                })?
        };

        let stdout_task = if let Some(stdout) = child.stdout.take() {
            Some(tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                let mut out = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!("stdout: {line}");
                    out.push(line);
                }
                out
            }))
        } else {
            tracing::warn!("Command has no stdout");
            None
        };

        let stderr_task = if let Some(stderr) = child.stderr.take() {
            Some(tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                let mut out = Vec::new();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!("stderr: {line}");
                    out.push(line);
                }
                out
            }))
        } else {
            tracing::warn!("Command has no stderr");
            None
        };

        let wait_future = child.wait();
        let status = match timeout {
            Some(limit) => match time::timeout(limit, wait_future).await {
                Ok(result) => result.map_err(|e| {
                    tracing::error!(error = ?e, "Failed to wait for command");
                    Status::internal(format!("Failed to wait for command: {e:?}"))
                })?,
                Err(_) => {
                    tracing::warn!(?limit, "Command exceeded timeout; terminating");
                    if let Err(err) = child.start_kill() {
                        tracing::warn!(?err, "Failed to start kill on timed out command");
                    }
                    if let Err(err) = child.wait().await {
                        tracing::warn!(?err, "Failed to reap timed out command");
                    }

                    let (stdout_lines, stderr_lines) =
                        collect_process_output(stdout_task, stderr_task).await;
                    let stdout = stdout_lines.join("\n");
                    let stderr = stderr_lines.join("\n");
                    let combined = merge_output(&stdout, &stderr);

                    let message = if combined.is_empty() {
                        format!("Command timed out after {limit:?}")
                    } else {
                        format!("Command timed out after {limit:?}: {combined}")
                    };

                    return Err(Status::deadline_exceeded(message));
                }
            },
            None => wait_future.await.map_err(|e| {
                tracing::error!(error = ?e, "Failed to wait for command");
                Status::internal(format!("Failed to wait for command: {e:?}"))
            })?,
        };

        let (stdout_lines, stderr_lines) = collect_process_output(stdout_task, stderr_task).await;
        let stdout = stdout_lines.join("\n");
        let stderr = stderr_lines.join("\n");

        let response = ShellResponse {
            exit_code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        };

        tracing::info!(command, exit_code = response.exit_code, "Command executed");

        Ok(Response::new(response))
    }
}

fn apply_env_settings(
    cmd: &mut Command,
    env_clear: bool,
    env_remove: Vec<String>,
    envs: HashMap<String, String>,
) {
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
}

fn is_background(cmd: &str) -> bool {
    let trimmed = cmd.trim_end();
    trimmed.ends_with('&') && !trimmed.ends_with("\\&")
}

async fn collect_process_output(
    stdout_task: Option<JoinHandle<Vec<String>>>,
    stderr_task: Option<JoinHandle<Vec<String>>>,
) -> (Vec<String>, Vec<String>) {
    let stdout = match stdout_task {
        Some(task) => match task.await {
            Ok(lines) => lines,
            Err(err) => {
                tracing::warn!(?err, "Failed to collect stdout from command");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    let stderr = match stderr_task {
        Some(task) => match task.await {
            Ok(lines) => lines,
            Err(err) => {
                tracing::warn!(?err, "Failed to collect stderr from command");
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    (stdout, stderr)
}

fn merge_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

#[cfg(test)]
mod tests {
    use super::codegen::shell_executor_server::ShellExecutor;
    use super::{MyShellExecutor, codegen::ShellRequest, is_background};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;
    use tonic::Request;

    #[test]
    fn test_is_background_basic() {
        assert!(is_background("echo hello &"));
    }

    #[test]
    fn test_is_background_trailing_spaces() {
        assert!(is_background("echo hello    &  "));
    }

    #[test]
    fn test_is_background_escaped_ampersand() {
        assert!(!is_background("echo hello \\&"));
    }

    #[test]
    fn test_is_not_background() {
        assert!(!is_background("echo hello"));
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_env_sh() {
        let executor = MyShellExecutor;
        let req = ShellRequest {
            command: "#!/usr/bin/env sh\necho shebang-env".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let resp = executor
            .exec_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.exit_code, 0);
        assert_eq!(resp.stdout.trim(), "shebang-env");
        assert!(resp.stderr.trim().is_empty());
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_direct_sh_with_args() {
        let executor = MyShellExecutor;
        let req = ShellRequest {
            command: "#!/bin/sh -eu\necho direct-sh".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let resp = executor
            .exec_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.exit_code, 0);
        assert_eq!(resp.stdout.trim(), "direct-sh");
        assert!(resp.stderr.trim().is_empty());
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_python3() {
        // Verify that a non-shell interpreter (python3) is used and executes Python syntax.
        let executor = MyShellExecutor;
        let req = ShellRequest {
            command: "#!/usr/bin/env python3\nprint('py-ok')".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let resp = executor
            .exec_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.exit_code, 0);
        assert_eq!(resp.stdout.trim(), "py-ok");
        assert!(resp.stderr.trim().is_empty());
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_bash_login_shell() {
        // The shebang path should be executed as a login shell so profile files are honored.
        if !Path::new("/bin/bash").exists() {
            return;
        }

        let home = tempdir().unwrap();
        fs::write(
            home.path().join(".bash_profile"),
            "export LOGIN_MARK=from_profile\n",
        )
        .unwrap();

        let executor = MyShellExecutor;
        let req = ShellRequest {
            command: "#!/bin/bash\nprintf \"%s\" \"${LOGIN_MARK:-missing}\"".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: [("HOME".into(), home.path().to_string_lossy().into_owned())]
                .into_iter()
                .collect(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let resp = executor
            .exec_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.exit_code, 0);
        assert_eq!(resp.stdout, "from_profile");
        assert!(resp.stderr.trim().is_empty());
    }
}
