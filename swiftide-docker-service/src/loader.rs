use std::pin::Pin;

use futures_util::{Stream, StreamExt as _, TryStreamExt};
use swiftide_core::Loader as _;
use swiftide_core::indexing::Node;
use swiftide_indexing::loaders::FileLoader;
use tonic::Status;

// The module `shell` is created by Tonic automatically because your
// package in shell.proto is named `shell`. The name "shell" below must
// match `package shell;` from shell.proto.
pub mod codegen {
    tonic::include_proto!("loader");
}

use codegen::loader_server::Loader;
use codegen::{LoadFilesRequest, NodeResponse};

#[derive(Debug, Default)]
pub struct MyLoaderExecutor;

#[tonic::async_trait]
impl Loader for MyLoaderExecutor {
    #[doc = " Server streaming response type for the LoadFiles method."]
    type LoadFilesStream = Pin<Box<dyn Stream<Item = Result<NodeResponse, Status>> + Send>>;

    #[doc = " Runs a shell command and returns exit code, stdout, and stderr."]
    async fn load_files(
        &self,
        request: tonic::Request<LoadFilesRequest>,
    ) -> Result<tonic::Response<Self::LoadFilesStream>, tonic::Status> {
        let args = request.into_inner();
        let loader = FileLoader::new(args.root_path).with_extensions(&args.file_extensions);

        Ok(tonic::Response::new(
            loader
                .into_stream()
                .map_ok(Into::into)
                .map_err(|err| tonic::Status::from_error(err.into()))
                .boxed(),
        ))
    }
}

impl From<Node> for NodeResponse {
    fn from(val: Node) -> Self {
        NodeResponse {
            path: val.path.to_string_lossy().to_string(),
            chunk: val.chunk,
            original_size: val.original_size as i32,
        }
    }
}
