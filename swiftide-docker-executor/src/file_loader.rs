use std::{borrow::Cow, path::PathBuf};

use codegen::{LoadFilesRequest, NodeResponse, loader_client::LoaderClient};
use swiftide_core::{Loader, indexing::TextNode};
use tokio::runtime::Handle;

use crate::RunningDockerExecutor;

pub mod codegen {
    tonic::include_proto!("loader");
}

#[derive(Debug, Clone)]
pub struct FileLoader<'a> {
    path: PathBuf,
    extensions: Vec<String>,
    executor: Cow<'a, RunningDockerExecutor>,
}

impl RunningDockerExecutor {
    /// Creates an owned file loader from the executor. If needed it is safe to clone the executor.
    ///
    /// The loader can be used with a swiftide indexing pipeline.
    pub fn into_file_loader<V: IntoIterator<Item = T>, T: Into<String>>(
        self,
        path: impl Into<PathBuf>,
        extensions: V,
    ) -> FileLoader<'static> {
        let path = path.into();
        let extensions = extensions.into_iter().map(Into::into).collect::<Vec<_>>();
        FileLoader {
            path,
            extensions,
            executor: Cow::Owned(self),
        }
    }

    /// Creates a borrowed file loader from the executor.
    pub fn as_file_loader<'a, V: IntoIterator<Item = T>, T: Into<String>>(
        &'a self,
        path: impl Into<PathBuf>,
        extensions: V,
    ) -> FileLoader<'a> {
        let path = path.into();
        let extensions = extensions.into_iter().map(Into::into).collect::<Vec<_>>();
        FileLoader {
            path,
            extensions,
            executor: Cow::Borrowed(self),
        }
    }
}

impl Loader for FileLoader<'_> {
    type Output = String;
    fn into_stream(self) -> swiftide_core::indexing::IndexingStream<String> {
        let container_ip = &self.executor.container_ip;
        let container_port = &self.executor.container_port;
        let mut client = tokio::task::block_in_place(|| {
            Handle::current().block_on(async {
                LoaderClient::connect(format!("http://{container_ip}:{container_port}")).await
            })
        })
        .expect("Failed to connect to service");

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<TextNode>>(1000);

        tokio::task::spawn(async move {
            let stream = match client
                .load_files(LoadFilesRequest {
                    root_path: self.path.to_string_lossy().to_string(),
                    file_extensions: self.extensions,
                })
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::error!(error = ?error, "Failed to load files");
                    return;
                }
            };

            let mut stream = stream.into_inner();

            while let Some(result) = stream.message().await.transpose() {
                if let Err(e) = tx
                    .send(
                        result
                            .map_err(anyhow::Error::from)
                            .and_then(TryInto::try_into),
                    )
                    .await
                {
                    tracing::error!(error = ?e, "error sending node");
                    break;
                }
            }
        });

        rx.into()
    }
}

impl TryInto<TextNode> for NodeResponse {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<TextNode, Self::Error> {
        TextNode::builder()
            .path(self.path)
            .chunk(self.chunk)
            .original_size(self.original_size as usize)
            .build()
    }
}
