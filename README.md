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
* Supports running *inside* Compose by self discovering the network

## Working directories

Commands execute inside `/app` by default. You can change the container-wide default with `.with_workdir("/path")` on the `DockerExecutor` builder:

```rust
let executor = DockerExecutor::default()
    .with_context_path(".")
    .with_image_name("test")
    .with_workdir("/tmp")
    .to_owned()
    .start()
    .await?;
```

Each command can further override its working directory via `Command::with_current_dir`. Relative paths are resolved against the executor's default workdir, while absolute paths are used verbatim:

```rust
let pwd = executor
    .exec_cmd(&Command::shell("pwd").with_current_dir("subproject"))
    .await?; // => /tmp/subproject

let tmp_pwd = executor
    .exec_cmd(&Command::shell("pwd").with_current_dir("/var/tmp"))
    .await?; // => /var/tmp
```

The same resolution logic applies to `Command::read_file` and `Command::write_file`, so relative file paths are always interpreted relative to the effective working directory for that command.

## Read-only shell commands

`Command::read_only_shell` uses a separate gRPC method from unrestricted shell execution. This is intentional: older services that do not implement the method return `UNIMPLEMENTED` instead of accidentally running the command through full shell access.

On Linux, the service runs read-only shell commands under a Landlock ruleset plus a small seccomp filter. Landlock handles write-like filesystem rights and grants them only to a per-command temporary `TMPDIR`; seccomp denies metadata mutation such as chmod, chown, and timestamp changes. Reads and command execution are left alone inside the executor container, while attempts to mutate the executor workspace fail. The command preserves the configured executor environment, including `HOME`, and defaults `GIT_OPTIONAL_LOCKS=0` so common read-only git commands avoid optional lock writes.

The high-level Docker executor tests exercise this through the normal gRPC path:

```sh
cargo test -p swiftide-docker-executor test_read_only_shell --locked -- --nocapture
```

Executors that cannot enforce these semantics return an error. They must not fall back to `Command::shell`.

## Timeouts

Long-running commands can be bounded either globally or per invocation. Set a default timeout that applies to every command with `.with_default_timeout(Duration)`:

```rust
use std::time::Duration;

let executor = DockerExecutor::default()
    .with_context_path(".")
    .with_image_name("test")
    .with_default_timeout(Duration::from_secs(5))
    .to_owned()
    .start()
    .await?;
```

Individual commands can override this value using the builder provided by `Command`:

```rust
use std::time::Duration;

let output = executor
    .exec_cmd(
        &Command::shell("sleep 10").with_timeout(Duration::from_secs(30)),
    )
    .await?;
```

If a command exceeds its timeout the future resolves with `CommandError::TimedOut`, including any partial output produced before the deadline. Calling `.clear_default_timeout()` removes the executor-level timeout entirely.

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
