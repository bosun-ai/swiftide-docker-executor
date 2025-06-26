use std::path::PathBuf;

use codegen::{loader_client::LoaderClient, LoadFilesRequest, NodeResponse};
use swiftide_core::{indexing::Node, Loader};
use tokio::runtime::Handle;

use crate::RunningDockerExecutor;

pub mod codegen {
    tonic::include_proto!("loader");
}

#[derive(Debug, Clone)]
pub struct FileLoader {
    path: PathBuf,
    extensions: Vec<String>,
    executor: RunningDockerExecutor,
}

impl RunningDockerExecutor {
    /// Creates a file loader from the executor. If needed it is safe to clone the executor.
    ///
    /// The loader can be used with a swiftide indexing pipeline.
    pub fn into_file_loader<V: IntoIterator<Item = T>, T: Into<String>>(
        self,
        path: impl Into<PathBuf>,
        extensions: V,
    ) -> FileLoader {
        let path = path.into();
        let extensions = extensions.into_iter().map(Into::into).collect::<Vec<_>>();
        FileLoader {
            path,
            extensions,
            executor: self,
        }
    }
}

impl Loader for FileLoader {
    fn into_stream(self) -> swiftide_core::indexing::IndexingStream {
        let host_port = &self.executor.host_port;
        let mut client = tokio::task::block_in_place(|| {
            Handle::current().block_on(async {
                LoaderClient::connect(format!("http://127.0.0.1:{host_port}")).await
            })
        })
        .expect("Failed to connect to Fluvio");

        let (tx, rx) = tokio::sync::mpsc::channel::<anyhow::Result<Node>>(1000);

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

impl TryInto<Node> for NodeResponse {
    type Error = anyhow::Error;

    fn try_into(self) -> Result<Node, Self::Error> {
        Node::builder()
            .path(self.path)
            .chunk(self.chunk)
            .original_size(self.original_size as usize)
            .build()
    }
}
