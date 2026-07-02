use std::{collections::HashMap, path::Path};

#[cfg(target_os = "linux")]
use std::{io::Write as _, os::unix::fs::PermissionsExt as _, process::Stdio};

use tokio::process::Child;
#[cfg(target_os = "linux")]
use tokio::process::Command;
use tonic::Status;

pub(crate) fn spawn_command(
    command: &str,
    workdir: &Path,
    temp_home: &Path,
    has_bash: bool,
    env_clear: bool,
    env_remove: Vec<String>,
    envs: HashMap<String, String>,
) -> Result<Child, Status> {
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
        // The child installs both sandbox layers before it runs the shell; the
        // service process is not restricted.
        unsafe {
            cmd.pre_exec(move || {
                let Some(ruleset) = landlock_ruleset.take() else {
                    return Err(std::io::Error::other(
                        "Landlock read-only ruleset was already consumed",
                    ));
                };

                enforce_read_only_landlock(ruleset)?;
                install_metadata_mutation_seccomp()
            });
        }

        cmd.spawn().map_err(|e| {
            tracing::error!(error = ?e, "Failed to start read-only command");
            Status::internal(format!("Failed to start read-only command: {e:?}"))
        })
    }
}

#[cfg(target_os = "linux")]
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

    let abi = ABI::V3;
    let write_access = AccessFs::from_write(abi);

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(write_access)
        .map_err(sandbox_error)?
        .create()
        .map_err(sandbox_error)?
        .add_rule(PathBeneath::new(
            PathFd::new(temp_home).map_err(sandbox_error)?,
            write_access,
        ))
        .map_err(sandbox_error)
}

#[cfg(target_os = "linux")]
fn enforce_read_only_landlock(ruleset: landlock::RulesetCreated) -> std::io::Result<()> {
    use landlock::RulesetStatus;

    let status = ruleset.restrict_self().map_err(sandbox_error)?;

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
fn install_metadata_mutation_seccomp() -> std::io::Result<()> {
    let mut filter = Vec::with_capacity(metadata_mutation_syscalls().len() * 2 + 2);
    filter.push(bpf_stmt(
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        std::mem::offset_of!(libc::seccomp_data, nr) as u32,
    ));

    for syscall in metadata_mutation_syscalls() {
        filter.push(bpf_jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            *syscall as u32,
            0,
            1,
        ));
        filter.push(bpf_stmt(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ));
    }

    filter.push(bpf_stmt(
        (libc::BPF_RET | libc::BPF_K) as u16,
        libc::SECCOMP_RET_ALLOW,
    ));

    let mut program = libc::sock_fprog {
        len: filter
            .len()
            .try_into()
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?,
        filter: filter.as_mut_ptr(),
    };

    // Safety: prctl is called with documented SECCOMP arguments. The filter
    // pointer stays valid for the duration of the syscall.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        if libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &mut program as *mut libc::sock_fprog,
            0,
            0,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn bpf_stmt(code: u16, k: u32) -> libc::sock_filter {
    // Safety: libc constructs a plain BPF statement value from scalar inputs.
    unsafe { libc::BPF_STMT(code, k) }
}

#[cfg(target_os = "linux")]
fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    // Safety: libc constructs a plain BPF jump value from scalar inputs.
    unsafe { libc::BPF_JUMP(code, k, jt, jf) }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn metadata_mutation_syscalls() -> &'static [libc::c_long] {
    &[
        libc::SYS_chmod,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchmodat2,
        libc::SYS_chown,
        libc::SYS_fchown,
        libc::SYS_lchown,
        libc::SYS_fchownat,
        libc::SYS_utime,
        libc::SYS_utimes,
        libc::SYS_futimesat,
        libc::SYS_utimensat,
    ]
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn metadata_mutation_syscalls() -> &'static [libc::c_long] {
    &[
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_fchown,
        libc::SYS_fchownat,
        libc::SYS_utimensat,
    ]
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn metadata_mutation_syscalls() -> &'static [libc::c_long] {
    &[]
}

#[cfg(target_os = "linux")]
fn sandbox_error(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn detects_bash_shebangs() {
        assert!(is_bash_shebang("#!/bin/bash"));
        assert!(is_bash_shebang("#!/usr/bin/env bash -e"));
        assert!(!is_bash_shebang("#!/usr/bin/env python3"));
        assert!(!is_bash_shebang("echo not a shebang"));
    }

    #[test]
    fn parses_shebang_command_and_args() {
        assert_eq!(
            shebang_command("#!/usr/bin/env python3 -u"),
            Some(("/usr/bin/env", vec!["python3", "-u"]))
        );
        assert_eq!(shebang_command("echo nope"), None);
        assert_eq!(shebang_args("#!/usr/bin/env bash -e"), Some(vec!["-e"]));
        assert_eq!(shebang_args("#!/bin/bash -e"), Some(vec!["-e"]));
    }

    #[test]
    fn read_only_defaults_respect_explicit_env_settings() {
        let mut envs = HashMap::new();
        let env_remove = vec!["GIT_OPTIONAL_LOCKS".to_string()];

        assert!(should_set_read_only_default("TMPDIR", &env_remove, &envs));
        assert!(!should_set_read_only_default(
            "GIT_OPTIONAL_LOCKS",
            &env_remove,
            &envs
        ));

        envs.insert("TMPDIR".to_string(), "/custom-tmp".to_string());
        assert!(!should_set_read_only_default("TMPDIR", &env_remove, &envs));
    }

    #[test]
    fn metadata_filter_includes_chmod_and_chown_syscalls() {
        let syscalls = metadata_mutation_syscalls();

        assert!(syscalls.contains(&libc::SYS_chmod));
        assert!(syscalls.contains(&libc::SYS_chown));
        assert!(syscalls.contains(&libc::SYS_utimensat));
    }
}
