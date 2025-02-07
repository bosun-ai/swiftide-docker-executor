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

## How it works

The executor communicates with docker over a grpc client build in `swiftide-docker-service`. The service is published on docker hub.

This gives more control than just relying on shell execution and enables future expansion.

When given a dockerfile, the executer copies the service from the `swiftide-docker-service` image, then starts it. Any existing CMDs or ENTRYPOINTs are removed.

For convenience, the executor only works with Ubuntu based images.
