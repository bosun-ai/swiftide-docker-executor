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

## Read-only shell

`RunningDockerExecutor::exec_sandboxed_shell` runs a shell command in a read-only sandbox, through the `ExecSandboxedShell` RPC of `swiftide-docker-service`. Apart from the sandbox it behaves like a normal shell command: same background handling, working directory, environment and timeout.

```rust
let output = executor
    .exec_sandboxed_shell(&Command::shell("git log -5"), &mut |_| {})
    .await?;
```

The sandbox is applied before the shell starts, so `bash --login` profiles and all child processes run inside it:

* Reads and execution are allowed everywhere.
* Writes, including truncating, creating, removing and renaming, are denied everywhere except on `/dev/null`, `/dev/zero`, `/dev/full` and `/dev/tty`, and beneath `/tmp/read-only-shell`, which `TMPDIR`, `TMP`, `TEMP` and `TEMPDIR` point to.
* The rest of `/tmp` is readable but not writable, so **tools that hardcode `/tmp` fail with a permission error**. A sandboxed process can outlive its call and has the same uid as the service, so writing to all of `/tmp` would let it change what unsandboxed commands later run or read from there, such as the service's temporary scripts.
* TCP bind and connect are denied, so the shell cannot reach the service's own unauthenticated gRPC port or a rootless Docker daemon on localhost. Landlock handles `connect` and `bind`. A seccomp filter handles what Landlock ABI 4 misses: TCP fast open, multipath TCP and other socket families. The filter also denies io_uring and kills processes using 32-bit system calls.
* `no_new_privs` is set and all capabilities are cleared, so a root shell in a privileged pod cannot lift the sandbox.

It fails closed. If the kernel lacks Landlock ABI 4 (Linux 6.7), `/tmp/read-only-shell` is anything other than a directory owned by the current user with mode 0700, or any other step of applying the sandbox fails, the command does not run and the call returns `SandboxedShellError::SandboxUnavailable` naming the reason. An image that predates the RPC answers `UNIMPLEMENTED`, which surfaces as `SandboxedShellError::UnsupportedByImage`.

### What it does not protect against

This keeps an agent from changing the checkout. It is not a boundary against an agent that means harm, and it does not keep secrets in:

* **Unix domain sockets stay reachable.** Landlock ABI 4 does not restrict connecting to them. That includes a git credential cache socket, and so the git token. It also includes a mounted Docker socket, which gives full control of that Docker daemon and so escapes the sandbox. **Containers started by `DockerExecutor` have the host's Docker socket mounted by default.**
* **UDP and DNS stay open**, so data can leave over them.
* **Secrets stay readable**, in the environment and anywhere on disk.
* **Metadata is not protected.** Landlock does not restrict `chmod`, `chown`, `utimes` or extended attribute changes on files the user owns, so `touch` on an existing file and `chmod` succeed.
* **Signals are not restricted.** The shell can kill processes of the same user, including the service.
* **`listen()` without `bind()`** still gets an ephemeral TCP port.
* **`/tmp/read-only-shell` is not trusted.** Sandboxed commands share it, and a write-capable command of the same user can put things in it. A sandboxed shell can also `chmod` it, which makes later sandboxed calls fail closed until it is fixed.

Dropping capabilities also ends root's bypass of file permissions: in the sandbox, root cannot read files whose mode bits deny it. And tools that insist on writing elsewhere, for instance to a cache in `HOME`, fail or need pointing elsewhere.

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
