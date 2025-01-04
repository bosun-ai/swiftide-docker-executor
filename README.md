# Swiftide Docker Tool Executor

A Tool executor meant to be used with Swiftide Agents.

Process is two-staged. First, configure the executor, then start it. The started executor implements `ToolExecutor` and can then be used in agents.

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
