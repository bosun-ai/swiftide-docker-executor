use std::io::{self, ErrorKind};
use std::process::ExitStatus;

use libc::{EINTR, ESRCH, SIGKILL, kill, pid_t};
use tokio::process::Child;

const KILL_EINTR_RETRIES: usize = 3;

/// Owns a child process and terminates its process group when execution is cancelled.
pub(crate) struct CommandGuard {
    child: Option<Child>,
    process_group: Option<ProcessGroup>,
}

impl CommandGuard {
    pub(crate) fn new(child: Child) -> Self {
        let process_group = child.id().and_then(|pid| {
            ProcessGroup::from_child_pid(pid)
                .inspect_err(|err| {
                    tracing::warn!(pid, ?err, "Child PID cannot be used as a process group");
                })
                .ok()
        });

        Self {
            child: Some(child),
            process_group,
        }
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self
            .child
            .as_mut()
            .expect("command guard must own a child")
            .wait()
            .await?;
        self.child.take();
        self.process_group.take();
        Ok(status)
    }

    pub(crate) fn terminate(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };

        if let Some(process_group) = self.process_group.take() {
            match process_group.kill() {
                Ok(()) => {}
                Err(err) if err.raw_os_error() == Some(ESRCH) => {
                    tracing::debug!("Command process group is already gone");
                }
                Err(err) => {
                    tracing::warn!(?err, "Failed to kill command process group");
                }
            }
        }

        if let Err(err) = child.start_kill() {
            if err.kind() == ErrorKind::InvalidInput {
                tracing::debug!(?err, "Command child is already gone");
            } else {
                tracing::warn!(?err, "Failed to start command kill");
            }
        }

        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => tracing::debug!(?status, "Reaped command"),
                Err(err) => tracing::warn!(?err, "Failed to reap command"),
            }
        });
    }
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[derive(Clone, Copy)]
struct ProcessGroup {
    pid: pid_t,
}

impl ProcessGroup {
    fn from_child_pid(pid: u32) -> io::Result<Self> {
        let pid =
            pid_t::try_from(pid).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        Ok(Self { pid })
    }

    fn kill(self) -> io::Result<()> {
        let process_group = -self.pid;
        let mut interruptions = 0;

        loop {
            // SAFETY: `kill` takes integer process identifiers and does not dereference memory.
            // `process_group` is the negated PID of a child spawned with `process_group(0)`.
            let result = unsafe { kill(process_group, SIGKILL) };
            if result == 0 {
                return Ok(());
            }

            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(EINTR) {
                return Err(err);
            }

            interruptions += 1;
            if interruptions > KILL_EINTR_RETRIES {
                return Err(err);
            }
        }
    }
}
