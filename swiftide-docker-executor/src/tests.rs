use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use bollard::{query_parameters::InspectContainerOptions, secret::ContainerStateStatusEnum};
use swiftide_core::{Command, CommandError, Loader as _, ToolExecutor as _, indexing::TextNode};
use tokio_stream::StreamExt as _;

use crate::{DockerExecutor, DockerExecutorError};

// A much smaller busybox image for faster tests
const TEST_DOCKERFILE: &str = "Dockerfile.tests";
const TEST_DOCKERFILE_ALPINE: &str = "Dockerfile.alpine.tests";
const TEST_DOCKERFILE_ENTRYPOINT: &str = "Dockerfile.entrypoint.tests";

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_runs_docker_and_echos() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("tests")
        .to_owned()
        .start()
        .await
        .unwrap();

    assert!(executor.is_running().await, "Container should be running");

    let output = executor
        .exec_cmd(&Command::shell("echo hello"))
        .await
        .unwrap();

    assert_eq!(output.to_string(), "hello");

    let output = executor
        .exec_cmd(&Command::shell("which rg"))
        .await
        .unwrap();

    assert_eq!(output.to_string(), "/usr/bin/rg");

    let output = executor
        .exec_cmd(&Command::shell("rg Cargo.toml"))
        .await
        .unwrap();

    assert!(
        output.to_string().contains("src/tests.rs"),
        "{output} does not contain expected path"
    );

    let output = executor
        .exec_cmd(&Command::shell("fd Cargo.toml"))
        .await
        .unwrap();

    assert!(
        output.to_string().contains("Cargo.toml"),
        "{output} does not contain expected path"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_runs_on_alpine() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE_ALPINE)
        .with_context_path(".")
        .with_image_name("tests")
        .to_owned()
        .start()
        .await
        .unwrap();

    assert!(executor.is_running().await, "Container should be running");

    let output = executor
        .exec_cmd(&Command::shell("echo hello"))
        .await
        .unwrap();

    assert_eq!(output.to_string(), "hello");

    let output = executor
        .exec_cmd(&Command::shell("which rg"))
        .await
        .unwrap();

    assert_eq!(output.to_string(), "/usr/bin/rg");

    let output = executor
        .exec_cmd(&Command::shell("rg Cargo.toml"))
        .await
        .unwrap();

    assert!(
        output.to_string().contains("src/tests.rs"),
        "{output} does not contain expected path"
    );

    let output = executor
        .exec_cmd(&Command::shell("fd Cargo.toml"))
        .await
        .unwrap();

    assert!(
        output.to_string().contains("Cargo.toml"),
        "{output} does not contain expected path"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_context_present() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_image_name("tests")
        .to_owned()
        .start()
        .await
        .unwrap();

    let ls = executor.exec_cmd(&Command::shell("ls -a")).await.unwrap();

    assert!(
        ls.to_string().contains("Cargo.toml"),
        "Context did not contain `Cargo.toml`, actual:\n {ls}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_overrides_include_git_respects_ignore() {
    let context_path = tempfile::tempdir().unwrap();
    // add a docker ignore file with .git
    std::fs::write(
        context_path.path().join(".dockerignore"),
        ".git\nignored_file",
    )
    .unwrap();
    std::fs::write(context_path.path().join("ignored_file"), "hello").unwrap();

    std::process::Command::new("cp")
        .arg(TEST_DOCKERFILE)
        .arg(context_path.path().join("Dockerfile"))
        .output()
        .unwrap();

    std::process::Command::new("git")
        .arg("init")
        .current_dir(context_path.path())
        .output()
        .unwrap();

    let user_email = std::process::Command::new("git")
        .arg("config")
        .arg("user.email")
        .arg("\"kwaak@bosun.ai\"")
        .current_dir(context_path.path())
        .output()
        .unwrap();

    assert!(user_email.status.success(), "failed to set git user email");

    let user_name = std::process::Command::new("git")
        .arg("config")
        .arg("user.name")
        .arg("\"kwaak\"")
        .current_dir(context_path.path())
        .output()
        .unwrap();

    assert!(user_name.status.success(), "failed to set git user name");
    // Make an initial commit
    std::process::Command::new("git")
        .arg("add")
        .arg(".")
        .current_dir(context_path.path())
        .output()
        .unwrap();

    std::process::Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg("Initial commit")
        .current_dir(context_path.path())
        .output()
        .unwrap();

    let local_ls = std::process::Command::new("ls")
        .arg("-aRl")
        .current_dir(context_path.path())
        .output()
        .unwrap();

    let output = std::str::from_utf8(&local_ls.stdout).unwrap();
    eprintln!("Local LS:\n {output}");
    assert!(output.contains(".git"));

    let executor = DockerExecutor::default()
        .with_context_path(context_path.path())
        .with_dockerfile("Dockerfile")
        .with_image_name("tests-git")
        .to_owned()
        .start()
        .await
        .unwrap();

    let ls = executor.exec_cmd(&Command::shell("ls -aRl")).await.unwrap();

    eprintln!("Executor LS:\n {ls}");
    assert!(ls.to_string().contains(".git"));
    assert!(!ls.to_string().contains("README.md"));
    assert!(!ls.to_string().contains("target"));
    assert!(!ls.to_string().contains("ignored_file"));

    // read .git/HEAD to check if git works
    let git_head = executor
        .exec_cmd(&Command::shell("cat .git/HEAD"))
        .await
        .unwrap();

    assert!(git_head.to_string().contains("ref: refs/heads/"));

    // test git works
    let git_status = executor
        .exec_cmd(&Command::shell("git status"))
        .await
        .unwrap();

    eprintln!("{git_status}");

    // It's ignored so git will think it's deleted
    assert!(git_status.to_string().contains("deleted:    ignored_file"));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_write_and_read_file_with_quotes() {
    let content = r#"This is a "test" content with 'quotes' and special characters: \n \t"#;
    let path = Path::new("test_file.txt");

    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-files")
        .to_owned()
        .start()
        .await
        .unwrap();

    // Write the content to the file
    let _ = executor
        .exec_cmd(&Command::write_file(path, content))
        .await
        .unwrap();

    // Read the content from the file
    //
    let read_file = executor.exec_cmd(&Command::read_file(path)).await.unwrap();

    assert_eq!(content, read_file.output);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_write_and_read_file_markdown() {
    let content = r#"# Example

        ```rust
        fn main() {
            let hello = "world";
            println!("Hello, {}", hello);
            }
        ```

        ```shell
        $ cargo run
        ```"#;
    let path = Path::new("test_file.txt");

    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-files-md")
        .to_owned()
        .start()
        .await
        .unwrap();

    // Write the content to the file
    let _ = executor
        .exec_cmd(&Command::write_file(path, content))
        .await
        .unwrap();

    // Read the content from the file
    //
    let read_file = executor.exec_cmd(&Command::read_file(path)).await.unwrap();

    assert_eq!(content, read_file.output);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_assert_container_stopped_on_drop() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-drop")
        .to_owned()
        .start()
        .await
        .unwrap();

    let docker = executor.docker.clone();
    let container_id = executor.container_id.clone();

    // assert it started
    let container = docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
        .unwrap();
    assert_eq!(
        container.state.as_ref().unwrap().status,
        Some(ContainerStateStatusEnum::RUNNING)
    );

    // Send a command to the container so that it's doing something
    let result = executor
        .exec_cmd(&Command::shell("echo 'hello'"))
        .await
        .unwrap();
    assert_eq!(result.to_string(), "hello");

    let _ = executor.shutdown().await;

    // assert it stopped
    let container = match docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
    {
        // If it's gone already we're good
        Err(e) if e.to_string().contains("No such container") => {
            return;
        }
        Ok(container) => container,
        Err(e) => panic!("Error inspecting container: {e}"),
    };
    let status = container.state.as_ref().unwrap().status;
    assert!(
        status == Some(ContainerStateStatusEnum::REMOVING)
            || status == Some(ContainerStateStatusEnum::EXITED)
            || status == Some(ContainerStateStatusEnum::DEAD),
        "Unexpected container state: {status:?}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_assert_container_retain_on_drop() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-drop")
        .retain_on_drop(true)
        .to_owned()
        .start()
        .await
        .unwrap();

    let docker = executor.docker.clone();
    let container_id = executor.container_id.clone();

    // assert it started
    let container = docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
        .unwrap();
    assert_eq!(
        container.state.as_ref().unwrap().status,
        Some(ContainerStateStatusEnum::RUNNING)
    );

    // Send a command to the container so that it's doing something
    let result = executor
        .exec_cmd(&Command::shell("echo 'hello'"))
        .await
        .unwrap();
    assert_eq!(result.to_string(), "hello");
    let container_id = executor.container_id.clone();

    drop(executor);

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // assert it stopped
    let container = match docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
    {
        Ok(container) => container,
        Err(e) => panic!("Error inspecting container: {e}"),
    };
    let status = container.state.as_ref().unwrap().status;
    assert_eq!(status, Some(ContainerStateStatusEnum::RUNNING));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_assert_container_stopped_on_drop_entrypoint() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE_ENTRYPOINT)
        .with_context_path(".")
        .with_image_name("test-drop-entrypoint")
        .to_owned()
        .start()
        .await
        .unwrap();

    let docker = executor.docker.clone();
    let container_id = executor.container_id.clone();

    // assert it started
    let container = docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
        .unwrap();
    assert_eq!(
        container.state.as_ref().unwrap().status,
        Some(ContainerStateStatusEnum::RUNNING)
    );

    // Send a command to the container so that it's doing something
    let result = executor
        .exec_cmd(&Command::shell("echo 'hello'"))
        .await
        .unwrap();
    assert_eq!(result.to_string(), "hello");

    let _ = executor.shutdown().await;

    // assert it stopped
    let container = match docker
        .inspect_container(&container_id, None::<InspectContainerOptions>)
        .await
    {
        // If it's gone already we're good
        Err(e) if e.to_string().contains("No such container") => {
            return;
        }
        Ok(container) => container,
        Err(e) => panic!("Error inspecting container: {e}"),
    };
    let status = container.state.as_ref().unwrap().status;
    assert!(
        status == Some(ContainerStateStatusEnum::REMOVING)
            || status == Some(ContainerStateStatusEnum::EXITED)
            || status == Some(ContainerStateStatusEnum::DEAD),
        "Unexpected container state: {status:?}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_create_file_subdirectory_that_does_not_exist() {
    let content = r#"# Example

        ```rust
        fn main() {
            let hello = "world";
            println!("Hello, {}", hello);
            }
        ```

        ```shell
        $ cargo run
        ```"#;
    let path = Path::new("doesnot/exist/test_file.txt");

    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-files-missing-dir")
        .to_owned()
        .start()
        .await
        .unwrap();

    // Write the content to the file
    let _ = executor
        .exec_cmd(&Command::write_file(path, content))
        .await
        .unwrap();

    // Read the content from the file
    //
    let read_file = executor.exec_cmd(&Command::read_file(path)).await.unwrap();

    // Assert that the written content matches the read content
    assert_eq!(content, read_file.output);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_custom_dockerfile() {
    let context_path = tempfile::tempdir().unwrap();

    std::process::Command::new("cp")
        .arg("Dockerfile.tests")
        .arg(context_path.path().join("Dockerfile.custom"))
        .output()
        .unwrap();

    let executor = DockerExecutor::default()
        .with_context_path(context_path.path())
        .with_image_name("test-custom")
        .with_dockerfile("Dockerfile.custom")
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor
        .exec_cmd(&Command::shell("echo hello"))
        .await
        .unwrap();
    assert_eq!(output.to_string(), "hello");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_nullifies_cmd() {
    let context_path = tempfile::tempdir().unwrap();

    let mut dockerfile_content = std::fs::read_to_string("Dockerfile.tests").unwrap();

    // Add a cmd that will exit right away
    dockerfile_content.push('\n');
    dockerfile_content.push_str("CMD [\"sh\", \"-c\", \"exit 0\"]");

    // Now write it to the temp dir
    std::fs::write(context_path.path().join("Dockerfile"), dockerfile_content).unwrap();

    let executor = DockerExecutor::default()
        .with_dockerfile("Dockerfile")
        .with_context_path(context_path.path())
        .with_image_name("test-null-cmd")
        .with_dockerfile("Dockerfile")
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor
        .exec_cmd(&Command::shell("echo hello"))
        .await
        .unwrap();
    dbg!(executor.logs().await.unwrap());
    assert_eq!(output.to_string(), "hello");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_nullifies_entrypoint() {
    let context_path = tempfile::tempdir().unwrap();

    let mut dockerfile_content = std::fs::read_to_string("Dockerfile.tests").unwrap();

    // Add a cmd that will exit right away
    dockerfile_content.push('\n');
    dockerfile_content.push_str("ENTRYPOINT [\"sh\", \"-c\", \"exit 0\"]");

    // Now write it to the temp dir
    std::fs::write(context_path.path().join("Dockerfile"), dockerfile_content).unwrap();

    let executor = DockerExecutor::default()
        .with_dockerfile("Dockerfile")
        .with_context_path(context_path.path())
        .with_image_name("test-null-entrypoint")
        .with_dockerfile("Dockerfile")
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor
        .exec_cmd(&Command::shell("echo hello"))
        .await
        .unwrap();
    assert_eq!(output.to_string(), "hello");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_container_state() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-state")
        .to_owned()
        .start()
        .await
        .unwrap();

    let state = executor.container_state().await.unwrap();
    assert_eq!(state.status, Some(ContainerStateStatusEnum::RUNNING));
    assert!(executor.is_running().await);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_invalid_dockerfile() {
    let context_path = tempfile::tempdir().unwrap();

    let mut dockerfile_content = std::fs::read_to_string("Dockerfile.tests").unwrap();

    // Add a cmd that will exit right away
    dockerfile_content.push('\n');
    dockerfile_content.push_str("SHOULD GIVE AN ERROR");

    // Now write it to the temp dir
    std::fs::write(context_path.path().join("Dockerfile"), dockerfile_content).unwrap();

    let err = DockerExecutor::default()
        .with_context_path(context_path.path())
        .with_image_name("test-invalid")
        .with_dockerfile("Dockerfile")
        .to_owned()
        .start()
        .await
        .unwrap_err();

    let DockerExecutorError::ImageBuild(err) = err else {
        panic!("{err:#}");
    };

    assert!(err.to_string().contains("unknown instruction: SHOULD"));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_docker_logs_in_stdout() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-logs")
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor
        .exec_cmd(&Command::shell("echo hello"))
        .await
        .unwrap();

    dbg!(output.to_string());

    let logs = executor.logs().await.unwrap();

    let expected = "stdout: hello";
    assert!(
        logs.iter().any(|l| l.contains(expected)),
        "Logs:\n {}",
        logs.join("\n")
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_regression_complicated_dockerfile() {
    let dockerfile = r"
ARG RUST_VERSION=1.89-slim
FROM rust:${RUST_VERSION} as builder

RUN rustup component add clippy rustfmt

# Install tool dependencies for app and git/ssh for the workspace
RUN apt-get update && apt-get install -y --no-install-recommends \
  ripgrep fd-find git ssh curl  \
  protobuf-compiler \
  libprotobuf-dev \
  pkg-config libssl-dev iputils-ping \
  make \

  # Needed for copypasta (internal for kwaak)
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  && rm -rf /var/lib/apt/lists/* \
  && cp /usr/bin/fdfind /usr/bin/fd


COPY . /app

WORKDIR /app
    ";

    let context_path = tempfile::tempdir().unwrap();

    std::fs::write(context_path.path().join("Dockerfile"), dockerfile).unwrap();

    let executor = DockerExecutor::default()
        .with_dockerfile("Dockerfile")
        .with_context_path(context_path.path())
        .with_image_name("test-complicated")
        .to_owned()
        .start()
        .await
        .unwrap();

    assert!(executor.is_running().await);
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_existing_image_no_context() {
    let executor = DockerExecutor::default()
        .with_existing_image("bosunai/swiftide-docker-service:latest")
        .to_owned()
        .start()
        .await
        .unwrap();

    assert!(executor.is_running().await);

    let output = executor.exec_cmd(&Command::shell("ls")).await;
    // assert_eq!(output.to_string(), "hello");
    println!("--- container logs ---");
    for log in executor.logs().await.unwrap() {
        println!("{log}");
    }
    output.unwrap();
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_loading_files() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("tests")
        .to_owned()
        .start()
        .await
        .unwrap();

    assert!(executor.is_running().await, "Container should be running");

    let loader = executor.into_file_loader("src/".to_string(), vec!["rs".to_string()]);
    let loader_stream = loader.into_stream();

    // Collect the results from the stream and assert there's a bunch of rust files
    let files = loader_stream
        .collect::<Result<Vec<TextNode>>>()
        .await
        .unwrap();

    assert!(!files.is_empty(), "No files loaded");
    assert!(
        files.iter().any(|node| node.path.ends_with("tests.rs")),
        "Expected to find tests.rs in loaded files"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_run_multiline_bash_script() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-multiline-bash")
        .to_owned()
        .start()
        .await
        .unwrap();

    let script = r#"
        #!/usr/bin/env bash
        echo "line1"
        echo "line2"
        echo "line3"
    "#;

    let output = executor.exec_cmd(&Command::shell(script)).await.unwrap();

    let result = output.to_string();
    assert!(result.contains("line1"));
    assert!(result.contains("line2"));
    assert!(result.contains("line3"));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_run_python_script() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-python-script")
        .to_owned()
        .start()
        .await
        .unwrap();

    let script = r#"#!/usr/bin/env python3
print("hello from python")
print(1 + 2)"#;

    let output = executor.exec_cmd(&Command::shell(script)).await;

    dbg!(executor.logs().await.unwrap());
    let output = output.unwrap();

    let result = output.to_string();
    assert!(result.contains("hello from python"));
    assert!(result.contains("3"));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_clear_env() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-no-env")
        .clear_env()
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor.exec_cmd(&Command::shell("env")).await.unwrap();

    // Check that common host env vars are not present
    let env_output = output.to_string();
    dbg!(&env_output);
    assert!(!env_output.contains("HOME="), "HOME env propagated");
    assert!(!env_output.contains("HOSTNAME="), "HOSTNAME env propagated");
    assert!(!env_output.contains("PATH="), "PATH env propagated");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_remove_env() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-no-env")
        .remove_env("HOSTNAME")
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor.exec_cmd(&Command::shell("env")).await.unwrap();

    // Check that common host env vars are not present
    let env_output = output.to_string();
    dbg!(&env_output);
    dbg!(&executor.logs().await.unwrap());
    assert!(!env_output.contains("HOSTNAME="), "HOST env propagated");
    assert!(env_output.contains("HOME="), "HOME env not propagated");
    assert!(env_output.contains("PATH="), "PATH env not propagated");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_add_env() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-no-env")
        .with_env("TEST_ENV", "test_value")
        .with_envs([("ANOTHER_ENV".to_string(), "another_value".to_string())])
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor.exec_cmd(&Command::shell("env")).await.unwrap();

    // Check that common host env vars are not present
    let env_output = output.to_string();
    dbg!(&env_output);
    assert!(
        env_output.contains("TEST_ENV=test_value"),
        "TEST_ENV not set"
    );
    assert!(
        env_output.contains("ANOTHER_ENV=another_value"),
        "ANOTHER_ENV not set"
    );
    assert!(env_output.contains("HOME="), "HOME env not propagated");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_default_timeout_triggers() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-timeout-default")
        .with_default_timeout(Duration::from_secs(1))
        .to_owned()
        .start()
        .await
        .unwrap();

    assert_eq!(executor.default_timeout, Some(Duration::from_secs(1)));

    let result = executor.exec_cmd(&Command::shell("sleep 5")).await;
    let err = result.expect_err("command should time out");

    match err {
        CommandError::TimedOut { timeout, .. } => {
            assert_eq!(timeout, Duration::from_secs(1));
        }
        other => panic!("unexpected error: {other:#}"),
    }
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_per_command_timeout_overrides_default() {
    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-timeout-override")
        .with_default_timeout(Duration::from_millis(500))
        .to_owned()
        .start()
        .await
        .unwrap();

    assert_eq!(executor.default_timeout, Some(Duration::from_millis(500)));

    let result = executor
        .exec_cmd(&Command::shell("sleep 1").with_timeout(Duration::from_secs(2)))
        .await
        .unwrap();

    assert!(result.is_empty(), "Expected no output, got: {result}");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_logs_stream_returns_live_log_lines() {
    let executor = Arc::new(
        DockerExecutor::default()
            .with_dockerfile(TEST_DOCKERFILE)
            .with_context_path(".")
            .with_image_name("test-logs-stream")
            .with_env("RUST_LOG", "debug")
            .to_owned()
            .start()
            .await
            .unwrap(),
    );

    // First, spawn the log tail in a tokio task and gather the logs in a vector
    let executor_clone = executor.clone();
    let log_task = tokio::spawn(async move {
        let mut logs = vec![];
        let mut stream = executor_clone.logs_stream().await;

        while let Some(line) = stream.next().await {
            println!("Log line: {line:?}");
            match line {
                Ok(log_line) => {
                    logs.push(log_line.to_string());
                }
                Err(e) => {
                    eprintln!("Error reading log line: {e}");
                }
            }
        }
        logs
    });

    println!("Waiting for logs to be generated...");

    // Generate some logs
    executor
        .exec_cmd(&Command::shell("echo log1 && echo log2 && echo log3"))
        .await
        .unwrap();

    // Give some time for the logs to be processed
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // The log task won't complete unless we stop the executor
    let _ = executor.shutdown().await;

    // Stream logs and collect a few lines
    let collected_logs = log_task.await.unwrap();
    // Collect up to 10 log lines or until we find our expected output
    let log_joined = collected_logs.join("\n");
    assert!(
        log_joined.contains("log1") && log_joined.contains("log2") && log_joined.contains("log3"),
        "Expected logs not found in streamed output: {log_joined:?}"
    );
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_background_shell_command_returns_immediately() {
    use std::time::Instant;

    let executor = DockerExecutor::default()
        .with_dockerfile(TEST_DOCKERFILE)
        .with_context_path(".")
        .with_image_name("test-bg-cmd")
        .to_owned()
        .start()
        .await
        .unwrap();

    let start = Instant::now();

    let output = executor
        .exec_cmd(&Command::shell("sleep 2 &"))
        .await
        .unwrap();
    let elapsed = start.elapsed();

    // Assert that the response returned almost immediately (well before 2s)
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "Background command took too long: {elapsed:?}"
    );

    // Optionally, assert output for a friendly message
    assert!(
        output.to_string().contains("Background command started")
            || output.to_string().trim().is_empty(),
        "Unexpected output from background command: {}",
        output
    );

    // Should still be able to run foreground commands
    let echo = executor
        .exec_cmd(&Command::shell("echo done"))
        .await
        .unwrap();
    assert_eq!(echo.to_string(), "done");
}
