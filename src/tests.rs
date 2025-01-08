use std::path::Path;

use bollard::secret::ContainerStateStatusEnum;
use swiftide_core::{Command, ToolExecutor as _};

use crate::DockerExecutor;

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_runs_docker_and_echos() {
    let executor = DockerExecutor::default()
        .with_context_path(".")
        .with_image_name("tests")
        .to_owned()
        .start()
        .await
        .unwrap();

    let output = executor
        .exec_cmd(&Command::Shell("echo hello".to_string()))
        .await
        .unwrap();

    assert_eq!(output.to_string(), "hello");
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_context_present() {
    let executor = DockerExecutor::default()
        .with_context_path(".")
        .with_image_name("tests")
        .with_working_dir("/app")
        .to_owned()
        .start()
        .await
        .unwrap();

    // Verify that the working directory is set correctly
    // TODO: Annoying this needs to be updated when files change in the root. Think of something better.
    let ls = executor
        .exec_cmd(&Command::Shell("ls -a".to_string()))
        .await
        .unwrap();

    assert!(ls.to_string().contains("Cargo.toml"));
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_write_and_read_file_with_quotes() {
    let content = r#"This is a "test" content with 'quotes' and special characters: \n \t"#;
    let path = Path::new("test_file.txt");

    let executor = DockerExecutor::default()
        .with_context_path(".")
        .with_image_name("test-files")
        .with_working_dir("/app")
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
        .with_context_path(".")
        .with_image_name("test-files-md")
        .with_working_dir("/app")
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
        .with_context_path(".")
        .with_image_name("test-drop")
        .with_working_dir("/app")
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
        .with_context_path(".")
        .with_image_name("test-files-missing-dir")
        .with_working_dir("/app")
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
        .arg("Dockerfile")
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

    let mut dockerfile_content = std::fs::read_to_string("Dockerfile").unwrap();

    // Add a cmd that will exit right away
    dockerfile_content.push('\n');
    dockerfile_content.push_str("CMD [\"sh\", \"-c\", \"exit 0\"]");

    // Now write it to the temp dir
    std::fs::write(context_path.path().join("Dockerfile"), dockerfile_content).unwrap();

    let executor = DockerExecutor::default()
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

    let mut dockerfile_content = std::fs::read_to_string("Dockerfile").unwrap();

    // Add a cmd that will exit right away
    dockerfile_content.push('\n');
    dockerfile_content.push_str("ENTRYPOINT [\"sh\", \"-c\", \"exit 0\"]");

    // Now write it to the temp dir
    std::fs::write(context_path.path().join("Dockerfile"), dockerfile_content).unwrap();

    let executor = DockerExecutor::default()
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
