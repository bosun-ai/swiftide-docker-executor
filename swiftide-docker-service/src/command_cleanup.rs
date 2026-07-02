use std::io::{self, ErrorKind};

use libc::{EINTR, ESRCH, SIGKILL, kill, pid_t};
use tokio::process::Child;

const KILL_EINTR_RETRIES: usize = 3;

/// Cleans up a command and its process group.
pub(crate) struct CommandCleanup {
    child: Child,
    process_group: Option<ProcessGroup>,
}

impl CommandCleanup {
    /// Creates cleanup for a spawned command and its process group.
    pub(crate) fn new(child: Child) -> Self {
        let process_group = ProcessGroup::from_child(&child);

        Self {
            child,
            process_group,
        }
    }

    /// Signals the command for termination and continues reaping it in the background.
    pub(crate) fn terminate(mut self) {
        if let Some(process_group) = self.process_group {
            process_group.kill_or_log("timed out command");
        }

        if let Err(err) = self.child.start_kill() {
            if err.kind() == ErrorKind::InvalidInput {
                tracing::debug!(?err, "Timed out command child is already gone");
            } else {
                tracing::warn!(?err, "Failed to start kill on timed out command");
            }
        }

        self.reap_in_background();
    }

    fn reap_in_background(mut self) {
        tokio::spawn(async move {
            match self.child.wait().await {
                Ok(status) => {
                    tracing::debug!(?status, "Reaped timed out command");
                }
                Err(err) => {
                    tracing::warn!(?err, "Failed to reap timed out command");
                }
            }
        });
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ProcessGroup {
    pid: pid_t,
}

impl ProcessGroup {
    pub(crate) fn from_child(child: &Child) -> Option<Self> {
        child.id().and_then(|pid| match Self::from_child_pid(pid) {
            Ok(process_group) => Some(process_group),
            Err(err) => {
                tracing::warn!(pid, ?err, "Command PID cannot be used as a process group");
                None
            }
        })
    }

    pub(crate) fn kill_or_log(self, context: &str) {
        match self.kill() {
            Ok(()) => {}
            Err(err) if err.raw_os_error() == Some(ESRCH) => {
                tracing::debug!(context, "Command process group is already gone");
            }
            Err(err) => {
                tracing::warn!(?err, context, "Failed to kill command process group");
            }
        }
    }

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
