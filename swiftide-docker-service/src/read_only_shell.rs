use std::{collections::HashMap, path::Path};

use tokio::process::Child;
use tonic::Status;

use crate::command_cleanup::CommandCleanup;
#[cfg(target_os = "linux")]
use crate::read_only_sandbox;

pub(crate) struct ReadOnlyWorkdir {
    path: std::path::PathBuf,
}

impl ReadOnlyWorkdir {
    pub(crate) fn try_new(path: impl Into<std::path::PathBuf>) -> Result<Self, Status> {
        let path = path.into();
        let metadata = std::fs::metadata(&path).map_err(|e| {
            Status::failed_precondition(format!(
                "read-only shell workdir does not exist: {}: {e}",
                path.display()
            ))
        })?;

        if !metadata.is_dir() {
            return Err(Status::failed_precondition(format!(
                "read-only shell workdir is not a directory: {}",
                path.display()
            )));
        }

        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) struct CommandEnv {
    clear: bool,
    remove: Vec<String>,
    set: HashMap<String, String>,
}

impl CommandEnv {
    pub(crate) fn new(clear: bool, remove: Vec<String>, set: HashMap<String, String>) -> Self {
        Self { clear, remove, set }
    }
}

pub(crate) struct SpawnedReadOnlyCommand {
    child: Child,
    #[cfg(target_os = "linux")]
    _script_dir: Option<ScriptDir>,
}

impl SpawnedReadOnlyCommand {
    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn terminate(self) {
        CommandCleanup::new(self.child).terminate();
    }
}

pub(crate) fn spawn(
    command: &str,
    workdir: ReadOnlyWorkdir,
    env: CommandEnv,
) -> Result<SpawnedReadOnlyCommand, Status> {
    #[cfg(not(target_os = "linux"))]
    {
        let CommandEnv { clear, remove, set } = env;
        let _ = (command, workdir.path(), clear, remove, set);
        Err(Status::unimplemented(
            "read-only shell is only supported on Linux",
        ))
    }

    #[cfg(target_os = "linux")]
    {
        let has_bash = Path::new("/bin/bash").exists();
        let prepared = read_only_command(command, has_bash)?;
        let mut cmd = prepared.command;

        env.apply(&mut cmd);
        cmd.current_dir(workdir.path())
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        read_only_sandbox::apply_to_command(&mut cmd)
            .map_err(|e| Status::internal(format!("Failed to apply read-only sandbox: {e:?}")))?;

        let child = cmd.spawn().map_err(|e| {
            tracing::error!(error = ?e, "Failed to start read-only command");
            Status::internal(format!("Failed to start read-only command: {e:?}"))
        })?;

        Ok(SpawnedReadOnlyCommand {
            child,
            _script_dir: prepared.script_dir,
        })
    }
}

#[cfg(target_os = "linux")]
struct PreparedCommand {
    command: tokio::process::Command,
    script_dir: Option<ScriptDir>,
}

#[cfg(target_os = "linux")]
struct ScriptDir {
    inner: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl ScriptDir {
    fn new() -> Result<Self, Status> {
        use std::os::unix::fs::PermissionsExt as _;

        let inner = tempfile::Builder::new()
            .prefix("swiftide-readonly-script-")
            .tempdir_in("/tmp")
            .map_err(|e| {
                Status::internal(format!("Failed to create read-only script dir: {e:?}"))
            })?;
        std::fs::set_permissions(inner.path(), std::fs::Permissions::from_mode(0o700)).map_err(
            |e| Status::internal(format!("Failed to protect read-only script dir: {e:?}")),
        )?;

        Ok(Self { inner })
    }

    fn path(&self) -> &Path {
        self.inner.path()
    }
}

#[cfg(target_os = "linux")]
impl CommandEnv {
    fn apply(&self, cmd: &mut tokio::process::Command) {
        if self.clear {
            tracing::info!("clearing environment variables");
            cmd.env_clear();
        }

        for var in &self.remove {
            tracing::info!(var, "clearing environment variable");
            cmd.env_remove(var);
        }

        for (key, value) in &self.set {
            tracing::info!(key, "setting environment variable");
            cmd.env(key, value);
        }

        if self.should_set_default("GIT_OPTIONAL_LOCKS") {
            cmd.env("GIT_OPTIONAL_LOCKS", "0");
        }
    }

    fn should_set_default(&self, key: &str) -> bool {
        !self.set.contains_key(key) && !self.remove.iter().any(|var| var == key)
    }
}

#[cfg(target_os = "linux")]
fn read_only_command(command: &str, has_bash: bool) -> Result<PreparedCommand, Status> {
    use std::{io::Write as _, os::unix::fs::PermissionsExt as _};

    let lines = command.lines().collect::<Vec<_>>();

    if let Some(first_line) = lines.first()
        && first_line.starts_with("#!")
    {
        let script_dir = ScriptDir::new()?;
        let script_path = script_dir.path().join("script");
        let mut script = std::fs::File::create(&script_path)
            .map_err(|e| Status::internal(format!("Failed to create read-only script: {e:?}")))?;
        script
            .write_all(command.as_bytes())
            .map_err(|e| Status::internal(format!("Failed to write read-only script: {e:?}")))?;
        drop(script);

        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).map_err(
            |e| Status::internal(format!("Failed to set read-only script permissions: {e:?}")),
        )?;

        if has_bash && is_bash_shebang(first_line) {
            let mut cmd = tokio::process::Command::new("/bin/bash");
            cmd.arg("--login");
            if let Some(args) = shebang_args(first_line) {
                cmd.args(args);
            }
            cmd.arg(script_path);
            return Ok(PreparedCommand {
                command: cmd,
                script_dir: Some(script_dir),
            });
        }

        let (interpreter, args) = shebang_command(first_line)
            .ok_or_else(|| Status::internal(format!("Failed to parse shebang: {first_line}")))?;
        let mut cmd = tokio::process::Command::new(interpreter);
        cmd.args(args);
        cmd.arg(script_path);
        return Ok(PreparedCommand {
            command: cmd,
            script_dir: Some(script_dir),
        });
    }

    let shell = if has_bash { "/bin/bash" } else { "sh" };
    let mut cmd = tokio::process::Command::new(shell);
    if has_bash {
        cmd.arg("--login");
    }
    cmd.arg("-c").arg(command);
    Ok(PreparedCommand {
        command: cmd,
        script_dir: None,
    })
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn shebang_command(line: &str) -> Option<(&str, Vec<&str>)> {
    let command = line.strip_prefix("#!")?;
    let mut parts = command.split_whitespace();
    let interpreter = parts.next()?;

    Some((interpreter, parts.collect()))
}

#[cfg(target_os = "linux")]
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
