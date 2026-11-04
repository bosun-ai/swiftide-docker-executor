//! Read-only sandbox for shell commands, enforced by Linux Landlock.
//!
//! A sandbox can only be applied by the process it confines, and building a Landlock ruleset
//! allocates, which is unsound between `fork` and `exec` in the multi-threaded service. So the
//! service starts itself again through a hidden subcommand. That fresh, single-threaded process
//! applies the sandbox to itself and then replaces itself with the shell, so nothing about the
//! service's own process or its later commands changes.
//!
//! The launcher reports back over a pipe it receives as stdin, which it swaps for `/dev/null`
//! before the shell starts. The pipe closes without a report once `exec` has succeeded, so the
//! service knows the outcome before it reports anything to the client, also for background
//! commands, whose output nobody reads.
//!
//! See the `ExecSandboxedShell` documentation in `proto/shell.proto` for the policy and its limits.

use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Stdio;

use landlock::{
    ABI, Access as _, AccessFs, AccessNet, CompatLevel, Compatible as _, PathBeneath, PathFd,
    RestrictionStatus, Ruleset, RulesetAttr as _, RulesetCreated, RulesetCreatedAttr as _,
    RulesetStatus,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};
use tokio::process::Command;
use tonic::Status;

/// The hidden subcommand that turns the service binary into the launcher.
pub(crate) const LAUNCHER_SUBCOMMAND: &str = "__exec-in-read-only-sandbox";

/// ABI 4 is the first with TCP restrictions, without which the shell could reach the service's
/// own gRPC port and escape. Fixed rather than the newest available, so that the policy is the
/// same on every kernel.
const REQUIRED_ABI: ABI = ABI::V4;
const REQUIRED_ABI_VERSION: i64 = REQUIRED_ABI as i64;

const WRITABLE_DIRECTORY: &str = "/tmp";
const WRITABLE_DEVICES: [&str; 4] = ["/dev/null", "/dev/zero", "/dev/full", "/dev/tty"];

const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;

/// The x32 ABI shares the x86-64 architecture check but numbers its system calls differently, so
/// they need rules of their own. On other architectures this adds rules that never match.
const X32_SYSCALL_BIT: i64 = 0x4000_0000;
const X32_SENDMSG: i64 = 518;
const X32_SENDMMSG: i64 = 538;

const SANDBOX_FAILED: u8 = b'S';
const EXEC_FAILED: u8 = b'E';
const LAUNCHER_EXIT_CODE: i32 = 125;

/// How a shell command is confined.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Confinement {
    None,
    ReadOnlySandbox,
}

impl Confinement {
    /// A command that starts `program` under this confinement. Its arguments, working directory
    /// and environment are set as usual.
    pub(crate) fn command(self, program: &str) -> Command {
        match self {
            Self::None => Command::new(program),
            Self::ReadOnlySandbox => {
                // Not a path from argv or the environment: the binary may have moved, and this
                // is exactly the code that is running.
                let mut launcher = Command::new("/proc/self/exe");
                launcher.arg(LAUNCHER_SUBCOMMAND).arg(program);
                launcher
            }
        }
    }

    /// Prepares `command` to report whether it started. Must be called after its stdin is
    /// configured, and `command` must be dropped after spawning for the report to complete.
    pub(crate) fn expect_start_report(self, command: &mut Command) -> Result<StartReport, Status> {
        match self {
            Self::None => Ok(StartReport(None)),
            Self::ReadOnlySandbox => {
                let (reader, writer) = io::pipe().map_err(|err| {
                    Status::internal(format!("Failed to create the sandbox status pipe: {err}"))
                })?;
                command.stdin(Stdio::from(writer));
                Ok(StartReport(Some(reader)))
            }
        }
    }
}

/// What the launcher has to say about starting the command.
pub(crate) struct StartReport(Option<io::PipeReader>);

impl StartReport {
    /// Waits until the command has really started, and fails if it never will.
    pub(crate) async fn confirmed(self) -> Result<(), Status> {
        let Some(mut pipe) = self.0 else {
            return Ok(());
        };
        let report = tokio::task::spawn_blocking(move || {
            let mut report = Vec::new();
            pipe.read_to_end(&mut report).map(|_| report)
        })
        .await
        .map_err(|err| Status::internal(format!("Failed to read the sandbox status: {err}")))?
        .map_err(|err| Status::internal(format!("Failed to read the sandbox status: {err}")))?;

        match report.split_first() {
            None => Ok(()),
            Some((&SANDBOX_FAILED, reason)) => Err(Status::failed_precondition(format!(
                "The read-only sandbox could not be applied and the command did not run: {}",
                String::from_utf8_lossy(reason)
            ))),
            Some((_, reason)) => Err(Status::internal(format!(
                "Failed to start command: {}",
                String::from_utf8_lossy(reason)
            ))),
        }
    }
}

/// The launcher: applies the sandbox to this process, then becomes `args`. Never returns; it is
/// the whole process, running single-threaded with no runtime started.
pub(crate) fn launch(mut args: impl Iterator<Item = OsString>) -> ! {
    // `dup` sets close-on-exec, so the shell does not inherit the pipe.
    let Ok(mut status) = io::stdin().as_fd().try_clone_to_owned().map(File::from) else {
        std::process::exit(LAUNCHER_EXIT_CODE);
    };

    let (kind, reason) = match apply_read_only_sandbox() {
        Err(reason) => (SANDBOX_FAILED, reason),
        Ok(()) => match args.next() {
            None => (EXEC_FAILED, "the launcher got no program to run".into()),
            Some(program) => {
                let err = std::process::Command::new(&program)
                    .args(args)
                    .stdin(Stdio::null())
                    .exec();
                (EXEC_FAILED, format!("{}: {err}", program.to_string_lossy()))
            }
        },
    };

    let _ = status.write_all(&[kind]);
    let _ = status.write_all(reason.as_bytes());
    std::process::exit(LAUNCHER_EXIT_CODE);
}

fn apply_read_only_sandbox() -> Result<(), String> {
    require_landlock_abi()?;
    close_inherited_descriptors_on_exec()?;
    // Everything needing privileges or path lookups happens first; restricting comes last.
    let ruleset = read_only_ruleset().map_err(|err| format!("building the ruleset: {err}"))?;
    drop_capabilities()?;
    let status = ruleset
        .restrict_self()
        .map_err(|err| format!("enforcing the ruleset: {err}"))?;
    require_full_enforcement(&status)?;
    deny_tcp_around_landlock().map_err(|err| format!("installing the seccomp filter: {err}"))
}

/// Landlock ABI 4 guards the `connect` and `bind` calls on plain TCP sockets, which leaves ways to
/// open a TCP connection that it never sees. Each reaches a local listener, such as this service's
/// gRPC port, and so would escape the sandbox:
/// - TCP fast open connects from `sendto`, `sendmsg` or `sendmmsg` with `MSG_FASTOPEN`.
/// - Multipath TCP and SMC sockets are not TCP to Landlock, but fall back to TCP on the wire.
/// - io_uring can do all of the above, out of sight of seccomp, so it is denied altogether.
///
/// Socket families are an allowlist because others may offer similar routes, and raw sockets
/// already need a capability the shell no longer has. The filter's architecture check kills
/// anything using 32-bit system calls, whose numbers these rules do not cover.
fn deny_tcp_around_landlock() -> Result<(), Box<dyn Error>> {
    let fast_open = |flags_arg| -> Result<_, seccompiler::BackendError> {
        Ok(vec![SeccompRule::new(vec![SeccompCondition::new(
            flags_arg,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::MaskedEq(libc::MSG_FASTOPEN as u64),
            libc::MSG_FASTOPEN as u64,
        )?])?])
    };
    let socket_rule =
        |arg, op, value| SeccompCondition::new(arg, SeccompCmpArgLen::Dword, op, value as u64);
    let family_not_allowed = SeccompRule::new(
        [
            libc::AF_UNIX,
            libc::AF_INET,
            libc::AF_INET6,
            libc::AF_NETLINK,
        ]
        .into_iter()
        .map(|family| socket_rule(0, SeccompCmpOp::Ne, family))
        .collect::<Result<_, _>>()?,
    )?;
    let multipath_tcp =
        SeccompRule::new(vec![socket_rule(2, SeccompCmpOp::Eq, libc::IPPROTO_MPTCP)?])?;

    let rules = [
        (
            libc::SYS_socket,
            libc::SYS_socket,
            vec![family_not_allowed, multipath_tcp],
        ),
        (libc::SYS_sendto, libc::SYS_sendto, fast_open(3)?),
        (libc::SYS_sendmsg, X32_SENDMSG, fast_open(2)?),
        (libc::SYS_sendmmsg, X32_SENDMMSG, fast_open(3)?),
        (
            libc::SYS_io_uring_setup,
            libc::SYS_io_uring_setup,
            Vec::new(),
        ),
    ]
    .into_iter()
    .flat_map(|(native, x32, rules)| [(native, rules.clone()), (x32 | X32_SYSCALL_BIT, rules)]);

    let filter: BpfProgram = SeccompFilter::new(
        rules.collect(),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EACCES as u32),
        std::env::consts::ARCH.try_into()?,
    )?
    .try_into()?;
    seccompiler::apply_filter(&filter)?;
    Ok(())
}

fn require_landlock_abi() -> Result<(), String> {
    // SAFETY: with null attributes and the version flag the kernel only returns the ABI version.
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    let need = format!("Landlock ABI {REQUIRED_ABI_VERSION} or later (Linux 6.7) is required");
    if version >= REQUIRED_ABI_VERSION {
        return Ok(());
    }
    if version >= 0 {
        return Err(format!("{need}, this kernel has ABI {version}"));
    }
    let err = io::Error::last_os_error();
    Err(match err.raw_os_error() {
        Some(libc::ENOSYS) => format!("{need}, this kernel has no Landlock support"),
        Some(libc::EOPNOTSUPP) => format!("{need}, Landlock is disabled on this kernel"),
        _ => format!("{need}, checking for Landlock failed: {err}"),
    })
}

/// Landlock checks a file when it is opened, so an inherited writable descriptor would keep
/// working. Anything the service was started with beyond stdio must not reach the shell.
fn close_inherited_descriptors_on_exec() -> Result<(), String> {
    // SAFETY: takes no pointers; the libc wrapper is avoided because not every libc has it, which
    // includes the gcompat on Alpine images.
    let result = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if result != 0 {
        return Err(format!(
            "closing inherited file descriptors: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Hard requirement: anything the kernel cannot enforce is an error, never a weaker sandbox.
fn read_only_ruleset() -> Result<RulesetCreated, Box<dyn Error>> {
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(REQUIRED_ABI))?
        // Handling TCP without adding a port rule denies all TCP bind and connect.
        .handle_access(AccessNet::from_all(REQUIRED_ABI))?
        .create()?
        .add_rule(PathBeneath::new(
            PathFd::new("/")?,
            AccessFs::from_read(REQUIRED_ABI),
        ))?;

    // A writable path that does not exist needs no rule: leaving it out only denies more.
    if Path::new(WRITABLE_DIRECTORY).exists() {
        ruleset = ruleset.add_rule(PathBeneath::new(
            PathFd::new(WRITABLE_DIRECTORY)?,
            AccessFs::from_all(REQUIRED_ABI),
        ))?;
    }
    for device in WRITABLE_DEVICES.iter().filter(|d| Path::new(d).exists()) {
        ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(device)?, AccessFs::WriteFile))?;
    }
    Ok(ruleset)
}

/// Production pods are privileged. With capabilities a shell could lift the sandbox through
/// CAP_SYS_ADMIN or bpf, or get around it with raw sockets.
///
/// All sets go, not only ambient and bounding: on `exec`, root regains the union of the bounding
/// and inheritable sets, whatever `no_new_privs` says.
fn drop_capabilities() -> Result<(), String> {
    // First, because it needs the CAP_SETPCAP that clearing the other sets removes.
    clear_bounding_set()?;

    // SAFETY: takes no pointers.
    let result = unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        )
    };
    if result != 0 {
        return Err(capability_error("clearing the ambient set"));
    }

    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    let header: [u32; 2] = [LINUX_CAPABILITY_VERSION_3, 0];
    // Version 3 takes two sets of effective, permitted and inheritable words, all zero here.
    let no_capabilities = [0u32; 6];
    // SAFETY: both pointers are valid for the sizes that version 3 of this syscall reads.
    let result =
        unsafe { libc::syscall(libc::SYS_capset, header.as_ptr(), no_capabilities.as_ptr()) };
    if result != 0 {
        return Err(capability_error(
            "clearing the effective, permitted and inheritable sets",
        ));
    }
    Ok(())
}

fn clear_bounding_set() -> Result<(), String> {
    // The kernel answers EINVAL past its last capability, so new capabilities are covered too.
    // SAFETY: neither prctl takes pointers.
    for capability in 0.. {
        match unsafe { libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) } {
            0 => continue,
            1 => {}
            _ => return Ok(()),
        }
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) } != 0 {
            let denied = io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            // Only CAP_SETPCAP allows this, so an unprivileged service cannot clear the set. That
            // is harmless: with no_new_privs only root regains capabilities from it on exec.
            return if denied && !may_act_as_root() {
                Ok(())
            } else {
                Err(capability_error("clearing the bounding set"))
            };
        }
    }
    Ok(())
}

fn may_act_as_root() -> bool {
    let (mut real, mut effective, mut saved) = (0, 0, 0);
    // SAFETY: all three pointers are valid for writes.
    let result = unsafe { libc::getresuid(&mut real, &mut effective, &mut saved) };
    result != 0 || real == 0 || effective == 0 || saved == 0
}

fn capability_error(step: &str) -> String {
    format!(
        "dropping capabilities failed while {step}: {}",
        io::Error::last_os_error()
    )
}

fn require_full_enforcement(status: &RestrictionStatus) -> Result<(), String> {
    if status.ruleset != RulesetStatus::FullyEnforced || !status.no_new_privs {
        return Err(format!(
            "the kernel did not fully enforce the ruleset: {status:?}"
        ));
    }
    Ok(())
}
