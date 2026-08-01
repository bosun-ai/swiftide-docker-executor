use std::io::{self, ErrorKind};
use std::process::ExitStatus;

use process_wrap::tokio::ChildWrapper;

/// Terminates the process group if command supervision ends before completion.
pub(crate) struct CommandGuard {
    child: Option<Box<dyn ChildWrapper>>,
}

impl CommandGuard {
    pub(crate) fn new(child: Box<dyn ChildWrapper>) -> Self {
        Self { child: Some(child) }
    }

    /// Waits for the shell without waiting for redirected background children.
    pub(crate) async fn wait_for_shell(&mut self) -> io::Result<ExitStatus> {
        self.child
            .as_mut()
            .expect("command guard must own a child")
            .inner_mut()
            .wait()
            .await
    }

    pub(crate) fn disarm(&mut self) {
        self.child.take();
    }

    pub(crate) fn terminate(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };

        if let Err(error) = child.start_kill()
            && error.kind() != ErrorKind::InvalidInput
        {
            tracing::warn!(?error, "Failed to kill command process group");
        }

        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => tracing::debug!(?status, "Reaped command process group"),
                Err(error) => tracing::warn!(?error, "Failed to reap command process group"),
            }
        });
    }
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}
