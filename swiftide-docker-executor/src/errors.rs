use std::{convert::Infallible, path::StripPrefixError};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DockerExecutorError {
    #[error("error from bollard")]
    Docker(#[from] bollard::errors::Error),

    #[error("error building context for image")]
    Context(#[from] ContextError),

    #[error("container state missing for {0}")]
    ContainerStateMissing(String),

    #[error("error initializing client")]
    Init(#[from] ClientError),

    #[error("error with io {0}")]
    Io(#[from] std::io::Error),

    #[error("error transforming dockerfile")]
    Transform(#[from] MangleError),

    #[error("error starting container")]
    Start(anyhow::Error),

    #[error("Failed to build image: {0}")]
    ImageBuild(#[from] ImageBuildError),

    #[error("Failed to prepare dockerfile: {0}")]
    DockerfilePreparation(#[from] DockerfileError),

    #[error("Failed to start container: {0}")]
    ContainerStart(#[from] ContainerStartError),
}

#[derive(Error, Debug)]
pub enum ContextError {
    #[error("error while trying to ignore files")]
    FailedIgnore(#[from] ignore::Error),

    #[error("failed while walking files in context")]
    Walk(#[from] walkdir::Error),

    #[error("error reading file")]
    Io(#[from] std::io::Error),

    #[error("failed to convert to relative path")]
    RelativePath(#[from] StripPrefixError),
}

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("failed to initialize client")]
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
    #[error("Build failed: {0}")]
    BuildFailed(String),

    #[error("Invalid image name: {0}")]
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
