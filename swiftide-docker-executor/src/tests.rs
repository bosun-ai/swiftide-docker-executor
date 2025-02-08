use std::path::Path;

use bollard::secret::ContainerStateStatusEnum;
use swiftide_core::{Command, ToolExecutor as _};

use crate::{DockerExecutor, DockerExecutorError};

// A much smaller busybox image for faster tests
const TEST_DOCKERFILE: &str = "Dockerfile.tests";

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
        .exec_cmd(&Command::Shell("echo hello".to_string()))
        .await
        .unwrap();

    assert_eq!(output.to_string(), "hello");
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

    let ls = executor
        .exec_cmd(&Command::Shell("ls -a".to_string()))
        .await
        .unwrap();

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

    let ls = executor
        .exec_cmd(&Command::Shell("ls -aRl".to_string()))
        .await
        .unwrap();

    eprintln!("Executor LS:\n {ls}");
    assert!(ls.to_string().contains(".git"));
    assert!(!ls.to_string().contains("README.md"));
    assert!(!ls.to_string().contains("target"));
    assert!(!ls.to_string().contains("ignored_file"));

    // read .git/HEAD to check if git works
    let git_head = executor
        .exec_cmd(&Command::Shell("cat .git/HEAD".to_string()))
        .await
        .unwrap();

    assert!(git_head.to_string().contains("ref: refs/heads/"));

    // test git works
    let git_status = executor
        .exec_cmd(&Command::Shell("git status".to_string()))
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
    let container = docker.inspect_container(&container_id, None).await.unwrap();
    assert_eq!(
        container.state.as_ref().unwrap().status,
        Some(ContainerStateStatusEnum::RUNNING)
    );

    drop(executor);

    // assert it stopped
    let container = match docker.inspect_container(&container_id, None).await {
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
        panic!("{:#}", err);
    };

    assert!(err.to_string().contains("unknown instruction: SHOULD"));
}
