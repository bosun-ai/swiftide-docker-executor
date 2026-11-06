use std::{convert::Infallible, net::AddrParseError, path::StripPrefixError};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DockerExecutorError {
    #[error("error from docker: {0}")]
    Docker(#[from] bollard::errors::Error),

    #[error(transparent)]
    Context(#[from] ContextError),

    #[error("container state missing for: {0}")]
    ContainerStateMissing(String),

    #[error(transparent)]
    Init(#[from] ClientError),

    #[error("error with io {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Transform(#[from] MangleError),

    #[error("error starting container: {0}")]
    Start(anyhow::Error),

    #[error(transparent)]
    ImageBuild(#[from] ImageBuildError),

    #[error(transparent)]
    DockerfilePreparation(#[from] DockerfileError),

    #[error(transparent)]
    ContainerStart(#[from] ContainerStartError),
}

/// Why a sandboxed shell command did not run. In every case nothing was executed.
///
/// `RunningDockerExecutor::exec_sandboxed_shell` returns these inside
/// `CommandError::ExecutorError`; use `downcast_ref` to tell them apart.
#[derive(Error, Debug)]
pub enum SandboxedShellError {
    /// The image's service predates `ExecSandboxedShell`.
    #[error(
        "the command did not run: this image's swiftide-docker-service is too old for a \
         sandboxed shell (it answered UNIMPLEMENTED); rebuild the image with a newer service"
    )]
    UnsupportedByImage,

    /// The service is new enough but could not apply the sandbox, for example on a kernel without
    /// Landlock ABI 4. The service's message names the reason.
    #[error("{0}")]
    SandboxUnavailable(String),

    /// Only `Command::Shell` can be sandboxed.
    #[error("the command did not run: only shell commands can run in a sandboxed shell")]
    NotAShellCommand,
}

#[derive(Error, Debug)]
pub enum ContextError {
    #[error("error while trying to ignore files: {0}")]
    FailedIgnore(#[from] ignore::Error),

    #[error("failed while walking files in context: {0}")]
    Walk(#[from] walkdir::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to convert to relative path{0}")]
    RelativePath(#[from] StripPrefixError),

    #[error("error with custom dockerfile: {0}")]
    CustomDockerfile(String),
}

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("failed to connect to docker: {0}")]
    Init(bollard::errors::Error),
}

#[derive(Error, Debug)]
pub enum MangleError {
    #[error("Failed to read Dockerfile: {0}")]
    DockerfileReadError(std::io::Error), // #[]
    //
    #[error("invalid dockerfile")]
    InvalidDockerfile, // IoError(std::io::Error),
                       // Utf8Error(std::string::FromUtf8Error),
}

#[derive(Error, Debug)]
pub enum ImageBuildError {
    #[error("error compressing context: {0}")]
    Compression(std::io::Error),

    #[error("build failed: {0}")]
    BuildFailed(String),

    #[error("build error: {0}")]
    BuildError(String),

    #[error("invalid image name: {0}")]
    InvalidImageName(String),
}

#[derive(Error, Debug)]
pub enum DockerfileError {
    #[error("Failed to mangle dockerfile: {0}")]
    MangleError(#[from] MangleError),

    #[error("Failed to write temporary file: {0}")]
    TempFileError(#[from] std::io::Error),

    #[error("Invalid dockerfile path: {0}")]
    InvalidPath(String),
}

#[derive(Error, Debug)]
pub enum ContainerStartError {
    #[error("Failed to create container: {0}")]
    Creation(bollard::errors::Error),

    #[error("Failed to start container: {0}")]
    Start(bollard::errors::Error),

    #[error("Failed to get container port: {0}")]
    PortMapping(String),

    #[error("Error from logs: {0}")]
    Logs(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(#[from] AddrParseError),
}
impl From<Infallible> for DockerExecutorError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

impl From<Infallible> for ContextError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}
