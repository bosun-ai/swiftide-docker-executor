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
