use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time;
use tonic::{Request, Response, Status};

use crate::command_cleanup::CommandCleanup;
use crate::output_collector::OutputCollector;

const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Generated gRPC shell service types.
pub mod codegen {
    tonic::include_proto!("shell");
}

use codegen::shell_executor_server::ShellExecutor;
use codegen::{ReadOnlyShellRequest, ShellRequest, ShellResponse};

/// gRPC shell executor service implementation.
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
        let mut temp_script: Option<tempfile::TempDir> = None;
        let mut child = if let Some(first_line) = lines.first()
            && first_line.starts_with("#!")
        {
            tracing::info!("detected shebang; running as script");

            let script_dir = tempfile::Builder::new()
                .prefix("swiftide-script-")
                .tempdir_in("/tmp")
                .map_err(|e| Status::internal(format!("Failed to create temp script: {e:?}")))?;
            let script_path = script_dir.path().join("script");
            std::fs::write(&script_path, command.as_bytes())
                .map_err(|e| Status::internal(format!("Failed to write temp script: {e:?}")))?;
            let permissions = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).map_err(|e| {
                Status::internal(format!("Failed to set script permissions: {e:?}"))
            })?;
            temp_script = Some(script_dir);

            let mut cmd = if has_bash && is_bash_shebang(first_line) {
                // Bash scripts should run as login shells so profile files are honored.
                let mut cmd = Command::new("/bin/bash");
                cmd.arg("--login");
                if let Some(args) = shebang_args(first_line) {
                    cmd.args(args);
                }
                cmd.arg(&script_path);
                cmd
            } else {
                // Invoke the interpreter ourselves so Linux never execs a just-written temp file.
                let (interpreter, args) = shebang_command(first_line).ok_or_else(|| {
                    Status::internal(format!("Failed to parse shebang: {first_line}"))
                })?;
                let mut cmd = Command::new(interpreter);
                cmd.args(args);
                cmd.arg(&script_path);
                cmd
            };

            apply_env_settings(&mut cmd, env_clear, env_remove, envs);

            cmd.current_dir(workdir_path)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?
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
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    tracing::error!(error = ?e, "Failed to start command");
                    Status::internal(format!("Failed to start command: {e:?}"))
                })?
        };

        let output = OutputCollector::capture(&mut child);

        let wait_future = child.wait();
        let status = match timeout {
            Some(limit) => match time::timeout(limit, wait_future).await {
                Ok(result) => result.map_err(|e| {
                    tracing::error!(error = ?e, "Failed to wait for command");
                    Status::internal(format!("Failed to wait for command: {e:?}"))
                })?,
                Err(_) => {
                    tracing::warn!(?limit, "Command exceeded timeout; terminating");
                    CommandCleanup::new(child).terminate();

                    let (stdout_lines, stderr_lines) =
                        output.collect_with_timeout(OUTPUT_DRAIN_TIMEOUT).await;
                    let message = timeout_message(limit, &stdout_lines, &stderr_lines);

                    drop(temp_script);
                    return Err(Status::deadline_exceeded(message));
                }
            },
            None => wait_future.await.map_err(|e| {
                tracing::error!(error = ?e, "Failed to wait for command");
                Status::internal(format!("Failed to wait for command: {e:?}"))
            })?,
        };

        drop(temp_script);

        let (stdout_lines, stderr_lines) = output.collect().await;
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

    #[tracing::instrument(skip_all)]
    async fn exec_read_only_shell(
        &self,
        request: Request<ReadOnlyShellRequest>,
    ) -> Result<Response<ShellResponse>, Status> {
        let ReadOnlyShellRequest {
            command,
            env_clear,
            env_remove,
            envs,
            timeout_ms,
            cwd,
        } = request.into_inner();

        if is_background(&command) {
            return Err(Status::invalid_argument(
                "read-only shell does not support background commands",
            ));
        }

        let timeout = timeout_ms.map(Duration::from_millis);
        let workdir = cwd.unwrap_or_else(|| ".".to_string());
        let workdir_path = Path::new(&workdir);

        ensure_read_only_workdir(workdir_path)?;

        let temp_home = tempfile::Builder::new()
            .prefix("swiftide-readonly-")
            .tempdir_in("/tmp")
            .map_err(|e| Status::internal(format!("Failed to create read-only home: {e:?}")))?;
        std::fs::set_permissions(temp_home.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Status::internal(format!("Failed to protect read-only home: {e:?}")))?;

        let has_bash = Path::new("/bin/bash").exists();
        let mut child = spawn_read_only_command(
            &command,
            workdir_path,
            temp_home.path(),
            has_bash,
            env_clear,
            env_remove,
            envs,
        )?;

        let output = OutputCollector::capture(&mut child);
        let wait_future = child.wait();
        let status = match timeout {
            Some(limit) => match time::timeout(limit, wait_future).await {
                Ok(result) => result.map_err(|e| {
                    tracing::error!(error = ?e, "Failed to wait for read-only command");
                    Status::internal(format!("Failed to wait for read-only command: {e:?}"))
                })?,
                Err(_) => {
                    tracing::warn!(?limit, "Read-only command exceeded timeout; terminating");
                    CommandCleanup::new(child).terminate();

                    let (stdout_lines, stderr_lines) =
                        output.collect_with_timeout(OUTPUT_DRAIN_TIMEOUT).await;
                    let message = timeout_message(limit, &stdout_lines, &stderr_lines);

                    drop(temp_home);
                    return Err(Status::deadline_exceeded(message));
                }
            },
            None => wait_future.await.map_err(|e| {
                tracing::error!(error = ?e, "Failed to wait for read-only command");
                Status::internal(format!("Failed to wait for read-only command: {e:?}"))
            })?,
        };

        drop(temp_home);

        let (stdout_lines, stderr_lines) = output.collect().await;
        let response = ShellResponse {
            exit_code: status.code().unwrap_or(-1),
            stdout: stdout_lines.join("\n"),
            stderr: stderr_lines.join("\n"),
        };

        tracing::info!(
            command,
            exit_code = response.exit_code,
            "Read-only command executed"
        );

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

fn spawn_read_only_command(
    command: &str,
    workdir: &Path,
    temp_home: &Path,
    has_bash: bool,
    env_clear: bool,
    env_remove: Vec<String>,
    envs: HashMap<String, String>,
) -> Result<tokio::process::Child, Status> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            command, workdir, temp_home, has_bash, env_clear, env_remove, envs,
        );
        Err(Status::unimplemented(
            "read-only shell is only supported on Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = read_only_command(command, temp_home, has_bash)?;
        apply_env_settings(&mut cmd, env_clear, env_remove.clone(), envs.clone());
        apply_read_only_env_defaults(&mut cmd, &env_remove, &envs, temp_home);

        cmd.current_dir(workdir)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let landlock_ruleset = create_read_only_landlock(temp_home)?;
        let mut landlock_ruleset = Some(landlock_ruleset);

        // Safety: pre_exec runs in the child process after fork and before exec.
        // The ruleset is created before spawning; the child only restricts itself
        // before it runs the shell, so the service process is not affected.
        unsafe {
            cmd.pre_exec(move || {
                let Some(ruleset) = landlock_ruleset.take() else {
                    return Err(std::io::Error::other(
                        "Landlock read-only ruleset was already consumed",
                    ));
                };
                enforce_read_only_landlock(ruleset)
            });
        }

        cmd.spawn().map_err(|e| {
            tracing::error!(error = ?e, "Failed to start read-only command");
            Status::internal(format!("Failed to start read-only command: {e:?}"))
        })
    }
}

#[cfg(target_os = "linux")]
fn read_only_command(command: &str, temp_home: &Path, has_bash: bool) -> Result<Command, Status> {
    let lines = command.lines().collect::<Vec<_>>();

    if let Some(first_line) = lines.first()
        && first_line.starts_with("#!")
    {
        let script_path = temp_home.join("script");
        let mut script = std::fs::File::create(&script_path)
            .map_err(|e| Status::internal(format!("Failed to create read-only script: {e:?}")))?;
        script
            .write_all(command.as_bytes())
            .map_err(|e| Status::internal(format!("Failed to write read-only script: {e:?}")))?;
        drop(script);
        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).map_err(|e| {
            Status::internal(format!("Failed to set read-only script permissions: {e:?}"))
        })?;

        if has_bash && is_bash_shebang(first_line) {
            let mut cmd = Command::new("/bin/bash");
            cmd.arg("--login");
            if let Some(args) = shebang_args(first_line) {
                cmd.args(args);
            }
            cmd.arg(script_path);
            return Ok(cmd);
        }

        let (interpreter, args) = shebang_command(first_line)
            .ok_or_else(|| Status::internal(format!("Failed to parse shebang: {first_line}")))?;
        let mut cmd = Command::new(interpreter);
        cmd.args(args);
        cmd.arg(script_path);
        return Ok(cmd);
    }

    let shell = if has_bash { "/bin/bash" } else { "sh" };
    let mut cmd = Command::new(shell);
    if has_bash {
        cmd.arg("--login");
    }
    cmd.arg("-c").arg(command);
    Ok(cmd)
}

#[cfg(target_os = "linux")]
fn apply_read_only_env_defaults(
    cmd: &mut Command,
    env_remove: &[String],
    envs: &HashMap<String, String>,
    temp_home: &Path,
) {
    if should_set_read_only_default("TMPDIR", env_remove, envs) {
        cmd.env("TMPDIR", temp_home);
    }

    if should_set_read_only_default("GIT_OPTIONAL_LOCKS", env_remove, envs) {
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
    }
}

#[cfg(target_os = "linux")]
fn should_set_read_only_default(
    key: &str,
    env_remove: &[String],
    envs: &HashMap<String, String>,
) -> bool {
    !envs.contains_key(key) && !env_remove.iter().any(|var| var == key)
}

#[cfg(target_os = "linux")]
fn create_read_only_landlock(temp_home: &Path) -> std::io::Result<landlock::RulesetCreated> {
    use landlock::{
        ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
        RulesetCreatedAttr,
    };

    // ABI V3 includes the write-like operations agents commonly use to mutate a
    // checkout, including file writes, creates, deletes, renames, and truncation.
    let abi = ABI::V3;
    let write_access = AccessFs::from_write(abi);

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(write_access)
        .map_err(landlock_error)?
        .create()
        .map_err(landlock_error)?
        .add_rule(PathBeneath::new(
            PathFd::new(temp_home).map_err(landlock_error)?,
            write_access,
        ))
        .map_err(landlock_error)
}

#[cfg(target_os = "linux")]
fn enforce_read_only_landlock(ruleset: landlock::RulesetCreated) -> std::io::Result<()> {
    use landlock::RulesetStatus;

    let status = ruleset.restrict_self().map_err(landlock_error)?;

    if status.ruleset == RulesetStatus::FullyEnforced {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "Landlock read-only ruleset was not fully enforced: {:?}",
            status
        )))
    }
}

#[cfg(target_os = "linux")]
fn landlock_error(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn ensure_read_only_workdir(workdir: &Path) -> Result<(), Status> {
    let metadata = std::fs::metadata(workdir).map_err(|e| {
        Status::failed_precondition(format!(
            "read-only shell workdir does not exist: {}: {e}",
            workdir.display()
        ))
    })?;

    if !metadata.is_dir() {
        return Err(Status::failed_precondition(format!(
            "read-only shell workdir is not a directory: {}",
            workdir.display()
        )));
    }

    if metadata.permissions().mode() & 0o002 != 0 {
        return Err(Status::failed_precondition(format!(
            "read-only shell workdir is world-writable: {}",
            workdir.display()
        )));
    }

    Ok(())
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

fn timeout_message(limit: Duration, stdout: &[String], stderr: &[String]) -> String {
    let mut message = format!("Command timed out after {limit:?}");

    if !stdout.is_empty() || !stderr.is_empty() {
        message.push_str(": ");
        push_lines(&mut message, stdout);

        if !stdout.is_empty() && !stderr.is_empty() {
            message.push('\n');
        }

        push_lines(&mut message, stderr);
    }

    message
}

fn push_lines(message: &mut String, lines: &[String]) {
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            message.push('\n');
        }
        message.push_str(line);
    }
}

#[cfg(test)]
mod tests {
    use super::codegen::shell_executor_server::ShellExecutor;
    use super::{
        MyShellExecutor, codegen::ReadOnlyShellRequest, codegen::ShellRequest, is_background,
    };
    use indoc::indoc;
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
    async fn read_only_shell_rejects_background_commands() {
        let executor = MyShellExecutor;
        let req = ReadOnlyShellRequest {
            command: "sleep 60 &".to_string(),
            timeout_ms: Some(5_000),
            cwd: None,
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
        };

        let err = executor
            .exec_read_only_shell(Request::new(req))
            .await
            .unwrap_err();

        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn read_only_shell_requires_existing_workdir() {
        let executor = MyShellExecutor;
        let req = ReadOnlyShellRequest {
            command: "pwd".to_string(),
            timeout_ms: Some(5_000),
            cwd: Some("/definitely/not/a/real/workdir".to_string()),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
        };

        let err = executor
            .exec_read_only_shell(Request::new(req))
            .await
            .unwrap_err();

        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_only_shell_allows_reads_and_temp_writes_but_blocks_workdir_writes() {
        let workdir = tempdir().unwrap();
        let file_path = workdir.path().join("existing.txt");
        let outside_path = std::env::temp_dir().join(format!(
            "swiftide-docker-service-readonly-outside-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&outside_path);
        fs::write(&file_path, "original").unwrap();

        let executor = MyShellExecutor;
        let req = ReadOnlyShellRequest {
            command: format!(
                concat!(
                    "printf 'read='\n",
                    "cat existing.txt\n",
                    "printf '\\ntmp='\n",
                    "echo temp-ok > \"$TMPDIR/out\" && cat \"$TMPDIR/out\"\n",
                    "printf '\\nworkdir='\n",
                    "if echo changed > existing.txt; then echo allowed; else echo denied; fi\n",
                    "printf '\\noutside='\n",
                    "if echo changed > {outside}; then echo allowed; else echo denied; fi\n",
                    "printf '\\nfinal=' && cat existing.txt"
                ),
                outside = outside_path.display()
            ),
            timeout_ms: Some(5_000),
            cwd: Some(workdir.path().to_string_lossy().into_owned()),
            env_clear: false,
            env_remove: vec![],
            envs: Default::default(),
        };

        let resp = executor
            .exec_read_only_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.exit_code, 0);
        let lines = resp
            .stdout
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            vec![
                "read=original",
                "tmp=temp-ok",
                "workdir=denied",
                "outside=denied",
                "final=original"
            ]
        );
        assert_eq!(fs::read_to_string(file_path).unwrap(), "original");
        assert!(!outside_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_only_shell_preserves_env_home_and_shebang_behavior() {
        if !Path::new("/bin/bash").exists() {
            return;
        }

        let workdir = tempdir().unwrap();
        let home = workdir.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join(".bash_profile"),
            "export PROFILE_MARKER=profile\n",
        )
        .unwrap();

        let executor = MyShellExecutor;
        let req = ReadOnlyShellRequest {
            command: indoc! {r#"
                #!/bin/bash
                printf 'env=%s\nprofile=%s\nhome=%s' \
                  "$READ_ONLY_MARKER" "$PROFILE_MARKER" "$HOME"
            "#}
            .to_string(),
            timeout_ms: Some(5_000),
            cwd: Some(workdir.path().to_string_lossy().into_owned()),
            env_clear: false,
            env_remove: vec![],
            envs: [
                ("HOME".to_string(), home.to_string_lossy().into_owned()),
                ("READ_ONLY_MARKER".to_string(), "from-env".to_string()),
            ]
            .into_iter()
            .collect(),
        };

        let resp = executor
            .exec_read_only_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.exit_code, 0);
        assert_eq!(
            resp.stdout.lines().collect::<Vec<_>>(),
            vec![
                "env=from-env",
                "profile=profile",
                &format!("home={}", home.display())
            ]
        );
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
    async fn test_exec_shell_shebang_python3_multiline() {
        // Ensure multi-line Python scripts keep their body intact when piped to the interpreter.
        let executor = MyShellExecutor;
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

        let resp = executor
            .exec_shell(Request::new(req))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.exit_code, 0);
        assert_eq!(resp.stdout.trim(), "5\nline-0\nline-1");
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
