#[cfg(target_os = "linux")]
use {std::path::Path, tokio::process::Command};

#[cfg(target_os = "linux")]
pub(crate) fn apply_to_command(cmd: &mut Command, scratch: &Path) -> std::io::Result<()> {
    let landlock_ruleset = create_read_only_landlock(scratch)?;
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

    Ok(())
}

#[cfg(target_os = "linux")]
fn create_read_only_landlock(scratch: &Path) -> std::io::Result<landlock::RulesetCreated> {
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
            PathFd::new(scratch).map_err(sandbox_error)?,
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
    let syscalls = metadata_mutation_syscalls();
    if syscalls.is_empty() {
        return Err(std::io::Error::other(
            "read-only shell metadata seccomp is not supported on this architecture",
        ));
    }

    let mut filter = Vec::with_capacity(syscalls.len() * 2 + 2);
    filter.push(bpf_stmt(
        (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
        std::mem::offset_of!(libc::seccomp_data, nr) as u32,
    ));

    for syscall in syscalls {
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
