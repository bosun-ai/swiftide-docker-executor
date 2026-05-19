use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncRead};
use tokio::process::Child;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::JoinHandle;
use tokio::time;

/// Collects stdout and stderr from a spawned process without blocking command supervision.
pub(crate) struct OutputCollector {
    stdout: OutputReader,
    stderr: OutputReader,
}

impl OutputCollector {
    /// Starts background readers for the process stdout and stderr pipes.
    pub(crate) fn capture(child: &mut Child) -> Self {
        Self {
            stdout: OutputReader::capture(child.stdout.take(), "stdout"),
            stderr: OutputReader::capture(child.stderr.take(), "stderr"),
        }
    }

    /// Waits for both process streams to finish and returns stdout and stderr lines.
    pub(crate) async fn collect(self) -> (Vec<String>, Vec<String>) {
        tokio::join!(self.stdout.collect(), self.stderr.collect())
    }

    /// Waits for both process streams to finish, aborting readers that exceed `timeout`.
    pub(crate) async fn collect_with_timeout(
        self,
        timeout: Duration,
    ) -> (Vec<String>, Vec<String>) {
        tokio::join!(
            self.stdout.collect_with_timeout(timeout),
            self.stderr.collect_with_timeout(timeout)
        )
    }
}

struct OutputReader {
    name: &'static str,
    lines: UnboundedReceiver<String>,
    task: Option<JoinHandle<()>>,
}

impl OutputReader {
    fn capture<R>(stream: Option<R>, name: &'static str) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (sender, lines) = mpsc::unbounded_channel();
        let task = stream.map(|stream| {
            tokio::spawn(async move {
                let mut stream_lines = tokio::io::BufReader::new(stream).lines();

                while let Ok(Some(line)) = stream_lines.next_line().await {
                    tracing::info!("{name}: {line}");
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            })
        });

        if task.is_none() {
            tracing::warn!("Command has no {name}");
        }

        Self { name, lines, task }
    }

    async fn collect(mut self) -> Vec<String> {
        if let Some(task) = self.task.take()
            && let Err(err) = task.await
        {
            tracing::warn!(stream = self.name, ?err, "Failed to collect command output");
        }

        self.drain_lines()
    }

    async fn collect_with_timeout(mut self, timeout: Duration) -> Vec<String> {
        if let Some(mut task) = self.task.take() {
            tokio::select! {
                result = &mut task => {
                    if let Err(err) = result {
                        tracing::warn!(stream = self.name, ?err, "Failed to collect command output");
                    }
                }
                () = time::sleep(timeout) => {
                    tracing::warn!(
                        stream = self.name,
                        ?timeout,
                        "Timed out draining command output"
                    );
                    task.abort();
                    if let Err(err) = task.await
                        && !err.is_cancelled()
                    {
                        tracing::warn!(
                            stream = self.name,
                            ?err,
                            "Failed to abort command output reader"
                        );
                    }
                }
            }
        }

        self.drain_lines()
    }

    fn drain_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();

        while let Ok(line) = self.lines.try_recv() {
            lines.push(line);
        }

        lines
    }
}
