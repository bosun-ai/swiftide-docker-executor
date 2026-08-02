use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use process_wrap::tokio::{CommandWrap, ProcessGroup};
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::pipe;
use tokio::process::Command;
use tokio::time;
use tonic::{Request, Response, Status};

use crate::command_cleanup::CommandGuard;

const READ_BUFFER_SIZE: usize = 8 * 1024;
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Generated gRPC shell service types.
pub mod codegen {
    tonic::include_proto!("shell");
}

use codegen::shell_event::Event;
use codegen::shell_executor_server::ShellExecutor;
use codegen::{ShellEvent, ShellRequest};

/// gRPC shell executor service implementation.
#[derive(Debug, Default)]
pub struct MyShellExecutor;

#[tonic::async_trait]
impl ShellExecutor for MyShellExecutor {
    type ExecShellStream = Pin<Box<dyn Stream<Item = Result<ShellEvent, Status>> + Send>>;

    #[tracing::instrument(skip_all)]
    async fn exec_shell(
        &self,
        request: Request<ShellRequest>,
    ) -> Result<Response<Self::ExecShellStream>, Status> {
        Ok(Response::new(shell_events(request.into_inner())))
    }
}

fn shell_events(
    request: ShellRequest,
) -> Pin<Box<dyn Stream<Item = Result<ShellEvent, Status>> + Send>> {
    Box::pin(async_stream::try_stream! {
        let ShellRequest {
            command,
            env_clear,
            env_remove,
            envs,
            timeout_ms,
            cwd,
        } = request;

        let timeout = timeout_ms.map(Duration::from_millis);
        tracing::debug!(?timeout, "resolved timeout for shell request");
        tracing::info!(command, "Received command");

        let workdir = cwd.unwrap_or_else(|| ".".to_string());
        let has_bash = Path::new("/bin/bash").exists();

        if is_background(&command) {
            spawn_background_command(
                command,
                Path::new(&workdir),
                has_bash,
                env_clear,
                env_remove,
                envs,
            )?;
            yield shell_event(Event::Output(Bytes::from_static(b"Background command started")));
            yield shell_event(Event::ExitCode(0));
            return;
        }

        let (mut command_process, _temp_script) = build_command(
            &command,
            Path::new(&workdir),
            has_bash,
            env_clear,
            env_remove,
            envs,
        )?;
        let (sender, mut output) =
            pipe::pipe().map_err(|err| Status::internal(format!("Failed to create output pipe: {err}")))?;
        let stdout = sender
            .into_blocking_fd()
            .map_err(|err| Status::internal(format!("Failed to configure output pipe: {err}")))?;
        let stderr = stdout
            .try_clone()
            .map_err(|err| Status::internal(format!("Failed to clone output pipe: {err}")))?;
        command_process.stdout(stdout).stderr(stderr);

        let mut command_process = CommandWrap::from(command_process);
        command_process.wrap(ProcessGroup::leader());
        let child = command_process.spawn().map_err(|err| {
            tracing::error!(?err, "Failed to start command");
            Status::internal(format!("Failed to start command: {err}"))
        })?;
        drop(command_process);
        let mut process = CommandGuard::new(child);
        let mut read_buffer = [0_u8; READ_BUFFER_SIZE];
        let mut output_closed = false;
        let mut exit_status: Option<std::process::ExitStatus> = None;
        let deadline = async {
            match timeout {
                Some(limit) => time::sleep(limit).await,
                None => futures_util::future::pending().await,
            }
        };
        tokio::pin!(deadline);

        loop {
            if output_closed
                && let Some(status) = exit_status
            {
                let exit_code = status.code().unwrap_or(-1);
                tracing::info!(exit_code, "Command executed");
                process.disarm();
                yield shell_event(Event::ExitCode(exit_code));
                return;
            }

            let event: Result<ProcessEvent, Status> = tokio::select! {
                read = output.read(&mut read_buffer), if !output_closed => {
                    read.map(ProcessEvent::Output).map_err(|err| {
                        Status::internal(format!("Failed to read command output: {err}"))
                    })
                }
                status = process.wait_for_shell(), if exit_status.is_none() => {
                    status.map(ProcessEvent::Exit).map_err(|err| {
                        Status::internal(format!("Failed to wait for command: {err}"))
                    })
                }
                () = &mut deadline => {
                    Ok(ProcessEvent::Timeout)
                }
            };

            match event? {
                ProcessEvent::Output(0) => output_closed = true,
                ProcessEvent::Output(read) => {
                    tracing::info!(bytes = read, "Captured command output");
                    yield shell_event(Event::Output(Bytes::copy_from_slice(&read_buffer[..read])));
                }
                ProcessEvent::Exit(status) => {
                    exit_status = Some(status);
                }
                ProcessEvent::Timeout => {
                    let limit = timeout.expect("deadline only completes when configured");
                    tracing::warn!(?limit, "Command exceeded timeout; terminating");
                    process.terminate();

                    let drain_deadline = time::sleep(OUTPUT_DRAIN_TIMEOUT);
                    tokio::pin!(drain_deadline);
                    while !output_closed {
                        let read: Result<Option<usize>, Status> = tokio::select! {
                            read = output.read(&mut read_buffer) => {
                                read.map(Some).map_err(|err| {
                                    Status::internal(format!("Failed to read command output: {err}"))
                                })
                            }
                            () = &mut drain_deadline => Ok(None)
                        };
                        match read? {
                            Some(0) => output_closed = true,
                            Some(read) => {
                                tracing::info!(bytes = read, "Captured command output");
                                yield shell_event(Event::Output(Bytes::copy_from_slice(&read_buffer[..read])));
                            }
                            None => {
                                tracing::warn!("Timed out draining command output");
                                break;
                            }
                        }
                    }

                    yield shell_event(Event::TimedOutAfterMs(duration_to_millis(limit)));
                    return;
                }
            }
        }
    })
}

enum ProcessEvent {
    Output(usize),
    Exit(std::process::ExitStatus),
    Timeout,
}

fn spawn_background_command(
    command: String,
    workdir: &Path,
    has_bash: bool,
    env_clear: bool,
    env_remove: Vec<String>,
    envs: HashMap<String, String>,
) -> Result<(), Status> {
    tracing::info!("Running command in background");
    let mut process = Command::new(if has_bash { "/bin/bash" } else { "sh" });
    if has_bash {
        process.arg("--login");
    }
    apply_env_settings(&mut process, env_clear, env_remove, envs);
    process
        .arg("-c")
        .arg(command)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| Status::internal(format!("Failed to start background command: {err}")))?;
    Ok(())
}

fn build_command(
    command: &str,
    workdir: &Path,
    has_bash: bool,
    env_clear: bool,
    env_remove: Vec<String>,
    envs: HashMap<String, String>,
) -> Result<(Command, Option<tempfile::TempDir>), Status> {
    let first_line = command.lines().next();
    let mut temp_script = None;
    let mut process = if let Some(first_line) = first_line
        && first_line.starts_with("#!")
    {
        tracing::info!("detected shebang; running as script");
        let script_dir = tempfile::Builder::new()
            .prefix("swiftide-script-")
            .tempdir_in("/tmp")
            .map_err(|err| Status::internal(format!("Failed to create temp script: {err}")))?;
        let script_path = script_dir.path().join("script");
        std::fs::write(&script_path, command.as_bytes())
            .map_err(|err| Status::internal(format!("Failed to write temp script: {err}")))?;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|err| Status::internal(format!("Failed to set script permissions: {err}")))?;
        temp_script = Some(script_dir);

        if has_bash && is_bash_shebang(first_line) {
            let mut process = Command::new("/bin/bash");
            process.arg("--login");
            if let Some(args) = shebang_args(first_line) {
                process.args(args);
            }
            process.arg(script_path);
            process
        } else {
            let (interpreter, args) = shebang_command(first_line).ok_or_else(|| {
                Status::internal(format!("Failed to parse shebang: {first_line}"))
            })?;
            let mut process = Command::new(interpreter);
            process.args(args).arg(script_path);
            process
        }
    } else {
        tracing::info!("no shebang detected; running as command");
        let mut process = Command::new(if has_bash { "/bin/bash" } else { "sh" });
        if has_bash {
            process.arg("--login");
        }
        process.arg("-c").arg(command);
        process
    };

    apply_env_settings(&mut process, env_clear, env_remove, envs);
    process.current_dir(workdir).stdin(Stdio::null());
    Ok((process, temp_script))
}

fn shell_event(event: Event) -> ShellEvent {
    ShellEvent { event: Some(event) }
}

fn duration_to_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

fn is_bash_shebang(line: &str) -> bool {
    let Some(command) = line.strip_prefix("#!") else {
        return false;
    };

    let mut parts = command.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(interpreter), _) if interpreter.ends_with("/bash") || interpreter == "bash" => true,
        (Some(interpreter), Some(program))
            if interpreter.ends_with("/env")
                && (program == "bash" || program.ends_with("/bash")) =>
        {
            true
        }
        _ => false,
    }
}

fn shebang_command(line: &str) -> Option<(&str, Vec<&str>)> {
    let command = line.strip_prefix("#!")?;
    let mut parts = command.split_whitespace();
    let interpreter = parts.next()?;

    Some((interpreter, parts.collect()))
}

fn shebang_args(line: &str) -> Option<Vec<&str>> {
    let (interpreter, mut parts) = shebang_command(line)?;

    if interpreter.ends_with("/env") {
        if parts.is_empty() {
            return None;
        }
        parts.remove(0);
    }

    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::codegen::shell_event::Event;
    use super::codegen::shell_executor_server::ShellExecutor;
    use super::{MyShellExecutor, codegen::ShellRequest, is_background};
    use bytes::Bytes;
    use futures_util::StreamExt as _;
    use indoc::indoc;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::time;
    use tonic::Request;

    async fn execute(request: ShellRequest) -> (Vec<u8>, Event) {
        let mut events = MyShellExecutor
            .exec_shell(Request::new(request))
            .await
            .unwrap()
            .into_inner();
        let mut output = Vec::new();
        let mut result = None;

        while let Some(event) = events.next().await {
            match event.unwrap().event.unwrap() {
                Event::Output(bytes) => output.extend_from_slice(&bytes),
                event @ (Event::ExitCode(_) | Event::TimedOutAfterMs(_)) => {
                    result = Some(event);
                }
            }
        }

        (output, result.expect("shell stream must end with a result"))
    }

    fn process_exists(pid: i32) -> bool {
        // SAFETY: signal 0 does not send a signal and only checks whether the PID exists.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

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
    async fn streams_exact_output_in_pipe_order() {
        let request = ShellRequest {
            command:
                "printf 'first\\n'; sleep 0.05; printf 'second\\n' >&2; sleep 0.05; printf 'third\\n'"
                    .into(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let (output, outcome) = tokio::time::timeout(Duration::from_secs(10), execute(request))
            .await
            .expect("combined output pipe should close after the shell exits");

        assert_eq!(output, b"first\nsecond\nthird\n");
        assert_eq!(outcome, Event::ExitCode(0));
    }

    #[tokio::test]
    async fn streams_partial_output_before_timeout_result() {
        let request = ShellRequest {
            command: "printf before-timeout; sleep 30 & :".into(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(3_000),
            cwd: None,
        };

        let (output, outcome) = execute(request).await;

        assert_eq!(output, b"before-timeout");
        assert_eq!(outcome, Event::TimedOutAfterMs(3_000));
    }

    #[tokio::test]
    async fn streams_non_utf8_output_without_conversion() {
        let request = ShellRequest {
            command: "printf '\\377'".into(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let (output, outcome) = execute(request).await;

        assert_eq!(output, [0xff]);
        assert_eq!(outcome, Event::ExitCode(0));
    }

    #[tokio::test]
    async fn dropping_stream_terminates_children_after_the_shell_exits() {
        let directory = tempdir().unwrap();
        let pid_file = directory.path().join("child.pid");
        let request = ShellRequest {
            command: format!(
                "sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; printf ready",
                pid_file.display()
            ),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: None,
            cwd: None,
        };
        let mut events = MyShellExecutor
            .exec_shell(Request::new(request))
            .await
            .unwrap()
            .into_inner();

        let event = time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("command should become ready")
            .expect("stream should produce output")
            .unwrap();
        assert_eq!(
            event.event,
            Some(Event::Output(Bytes::from_static(b"ready")))
        );
        let child_pid = fs::read_to_string(pid_file).unwrap().parse().unwrap();

        drop(events);

        time::timeout(Duration::from_secs(5), async {
            while process_exists(child_pid) {
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("dropping the stream should terminate and reap the process group");
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_env_sh() {
        let req = ShellRequest {
            command: "#!/usr/bin/env sh\necho shebang-env".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let (output, outcome) = execute(req).await;
        assert_eq!(outcome, Event::ExitCode(0));
        assert_eq!(output, b"shebang-env\n");
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_direct_sh_with_args() {
        let req = ShellRequest {
            command: "#!/bin/sh -eu\necho direct-sh".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let (output, outcome) = execute(req).await;
        assert_eq!(outcome, Event::ExitCode(0));
        assert_eq!(output, b"direct-sh\n");
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_python3() {
        // Verify that a non-shell interpreter (python3) is used and executes Python syntax.
        let req = ShellRequest {
            command: "#!/usr/bin/env python3\nprint('py-ok')".to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let (output, outcome) = execute(req).await;
        assert_eq!(outcome, Event::ExitCode(0));
        assert_eq!(output, b"py-ok\n");
    }

    #[tokio::test]
    async fn test_exec_shell_shebang_python3_multiline() {
        // Ensure multi-line Python scripts keep their body intact when piped to the interpreter.
        let command = indoc! {r#"
            #!/usr/bin/env python3
            import sys




            def add(a, b):
                return a + b


            if __name__ == "__main__":
                print(add(2, 3))
                for i in range(2):
                    print(f"line-{i}")
        "#};
        let req = ShellRequest {
            command: command.to_string(),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
            timeout_ms: Some(5_000),
            cwd: None,
        };

        let (output, outcome) = execute(req).await;

        assert_eq!(outcome, Event::ExitCode(0));
        assert_eq!(output, b"5\nline-0\nline-1\n");
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

        let (output, outcome) = execute(req).await;

        assert_eq!(outcome, Event::ExitCode(0));
        assert_eq!(output, b"from_profile");
    }
}
