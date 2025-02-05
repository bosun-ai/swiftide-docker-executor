use std::{fs::read_to_string, path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use bollard::image::BuildImageOptions;
use swiftide_core::prelude::StreamExt as _;
use tracing::info;

use crate::{client::Client, DockerExecutorError, ImageBuildError};

pub struct ImageBuilder {
    docker: Arc<Client>,
}

impl ImageBuilder {
    pub fn new(docker: Arc<Client>) -> Self {
        Self { docker }
    }

    pub async fn build_image(
        &self,
        context_path: &Path,
        context: Vec<u8>,
        dockerfile: &Path,
        image_name: &str,
        tag: &str,
    ) -> Result<String, ImageBuildError> {
        let image_name_with_tag = format!("{image_name}:{tag}");

        let relative_dockerfile = dockerfile
            .canonicalize()
            .map_err(|e| ImageBuildError::InvalidImageName(e.to_string()))?
            .strip_prefix(
                std::fs::canonicalize(context_path)
                    .map_err(|e| ImageBuildError::InvalidImageName(e.to_string()))?,
            )
            .map_err(|e| ImageBuildError::InvalidImageName(e.to_string()))?
            .to_path_buf();

        let build_options = BuildImageOptions {
            t: image_name_with_tag.as_str(),
            rm: true,
            dockerfile: &relative_dockerfile.to_string_lossy(),
            ..Default::default()
        };

        let mut build_stream = self
            .docker
            .build_image(build_options, None, Some(context.into()));

        while let Some(log) = build_stream.next().await {
            match log {
                Ok(output) => {
                    if let Some(stream) = output.stream {
                        tracing::debug!("Build log: {}", stream);
                    }
                }
                Err(e) => {
                    return Err(ImageBuildError::BuildFailed(e.to_string()));
                }
            }
        }

        Ok(image_name_with_tag)
    }
}
