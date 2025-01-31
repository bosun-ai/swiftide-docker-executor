use std::{convert::Infallible, path::StripPrefixError};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum DockerExecutorError {
    #[error("error from bollard")]
    Bollard(#[from] bollard::errors::Error),

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
                                         // IoError(std::io::Error),
                                         // Utf8Error(std::string::FromUtf8Error),
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
