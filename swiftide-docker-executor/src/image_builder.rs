use std::{io::Write as _, path::Path, sync::Arc};

use anyhow::Result;
use bollard::{image::BuildImageOptions, secret::BuildInfoAux};
use swiftide_core::prelude::StreamExt as _;

use crate::{client::Client, ImageBuildError};

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
        let mut c = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        c.write_all(&context)
            .map_err(ImageBuildError::Compression)?;
        let compressed = c.finish().map_err(ImageBuildError::Compression)?;

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
            session: Some(image_name_with_tag.to_string()),
            ..Default::default()
        };

        let mut build_stream =
            self.docker
                .build_image(build_options, None, Some(compressed.into()));

        let mut logs = Vec::new();
        while let Some(log) = build_stream.next().await {
            match log {
                Ok(output) => {
                    if let Some(output) = output.stream {
                        logs.push(output.clone());
                        tracing::info!("{}", output);
                    }

                    // TODO: Verify to_string() is good enough
                    if let Some(BuildInfoAux::BuildKit(inner)) = output.aux {
                        logs.push(inner.to_string());
                        tracing::info!("{}", inner);
                    }

                    if let Some(error) = output.error {
                        let details = output
                            .error_detail
                            .and_then(|e| e.message)
                            .unwrap_or_default();

                        tracing::error!(details, "Build error: {error}");

                        return Err(ImageBuildError::BuildError(format!("{error} {details}")));
                    }
                }
                Err(e) => {
                    if let bollard::errors::Error::DockerStreamError { error } = e {
                        return Err(ImageBuildError::BuildFailed(format!(
                            "error during build: {error}"
                        )));
                    }
                    return Err(ImageBuildError::BuildFailed(e.to_string()));
                }
            }
        }

        Ok(image_name_with_tag)
    }
}
