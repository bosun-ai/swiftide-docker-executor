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
}

#[derive(Error, Debug)]
pub enum ContextError {
    #[error("error iterating over directory")]
    DirWalker(#[from] ignore::Error),

    #[error("error reading file")]
    Io(#[from] std::io::Error),

    #[error("failed to convert to relative path")]
    RelativePath(#[from] StripPrefixError),
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
