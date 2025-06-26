use std::{io::Write as _, sync::Arc};

use anyhow::Result;
use bollard::query_parameters::BuildImageOptions;
#[cfg(feature = "buildkit")]
use bollard::secret::BuildInfoAux;
use http_body_util::{Either, Full};
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
        context: Vec<u8>,
        dockerfile: &str,
        image_name: &str,
        tag: &str,
    ) -> Result<String, ImageBuildError> {
        let mut c = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        c.write_all(&context)
            .map_err(ImageBuildError::Compression)?;
        let compressed = c.finish().map_err(ImageBuildError::Compression)?;

        let image_name_with_tag = format!("{image_name}:{tag}");

        let build_options = BuildImageOptions {
            t: Some(image_name_with_tag.clone()),
            rm: true,
            dockerfile: dockerfile.to_string(),
            #[cfg(feature = "buildkit")]
            version: bollard::query_parameters::BuilderVersion::BuilderBuildKit,
            #[cfg(feature = "buildkit")]
            session: Some(image_name_with_tag.to_string()),
            ..Default::default()
        };

        let mut build_stream = self.docker.build_image(
            build_options,
            None,
            Some(Either::Left(Full::new(compressed.into()))),
        );

        while let Some(log) = build_stream.next().await {
            match log {
                Ok(output) => {
                    if let Some(output) = output.stream {
                        tracing::info!("{}", output);
                    }

                    // TODO: Verify to_string() is good enough
                    #[cfg(feature = "buildkit")]
                    if let Some(BuildInfoAux::BuildKit(inner)) = output.aux {
                        inner
                            .vertexes
                            .iter()
                            .take(1)
                            .for_each(|log| tracing::info!("{}", log.name))
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
