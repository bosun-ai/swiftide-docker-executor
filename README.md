# Swiftide Docker Tool Executor

A Tool executor meant to be used with Swiftide Agents.

Process is two-staged. First, configure the executor, then start it. The started executor implements `ToolExecutor` and can then be used in agents.

This executor is used mainly in [kwaak](https://github.com/bosun-ai/kwaak). It is set up generically, is useable as an executor for any swiftide agent.

## Usage

```rust

let executor = DockerExecutor::default()
    .with_context_path(".")
    .with_image_name("test")
    .with_dockerfile("Dockerfile.overwritten");

executor

let context = DefaultContext::from_executor(executor);

let agent = Agent::builder().context(context).build();
```

## Features

* Execute swiftide tools in a docker container
* Automagically works with local docker images, or roll your own and make sure the `swiftide-docker-service` is in the path
* GRPC based communication
* The service is published on docker hub, and can also be used in other contexts (i.e. kubernetes)
* Indexing files streaming, remotely, into a Swiftide indexing pipeline
* Opt-in buildkit for faster builds

## Loading files into a Swiftide indexing pipeline

Additionally, the executor can be used to load files into a Swiftide indexing pipeline.

```rust
let executor = DockerExecutor::default()
    .with_context_path(".")
    .with_image_name("test")
    .with_dockerfile("Dockerfile.overwritten");

let loader = executor.into_file_loader("./", vec![".rs"]);

swiftide::indexing::from_loader(loader)
```

## How it works

The executor communicates with docker over a grpc client build in `swiftide-docker-service`. The service is published on docker hub.

This gives more control than just relying on shell execution and enables future expansion.

When given a dockerfile, the executer copies the service from the `swiftide-docker-service` image, then starts it. Any existing CMDs or ENTRYPOINTs are removed.

For convenience, the executor only works with Ubuntu based images.
