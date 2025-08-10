/// Adds copy statements to the Dockerfile to copy the built binary into the image.
use std::path::Path;

use tokio::fs::read_to_string;

use crate::MangleError;

pub struct MangledDockerfile {
    pub content: String,
}

pub async fn mangle(path: &Path) -> Result<MangledDockerfile, MangleError> {
    tracing::warn!("Mangling Dockerfile at {:?}", path);

    let this_crate = env!("CARGO_PKG_VERSION");
    let mut content = read_to_string(path)
        .await
        .map_err(MangleError::DockerfileReadError)?;

    let image_name = format!("bosunai/swiftide-docker-service:{this_crate}");
    // Remove existing CMD or ENTRYPOINT instructions
    content = content
        .lines()
        .filter(|line| {
            !line.trim_start().to_lowercase().starts_with("cmd")
                && !line.trim_start().to_lowercase().starts_with("entrypoint")
        })
        .collect::<Vec<&str>>()
        .join("\n");

    // Find the position to insert the copy lines
    let mut lines = content.lines().collect::<Vec<_>>();
    let insert_pos = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().to_lowercase().starts_with("from"))
        .next_back()
        .map(|(idx, _)| idx)
        .ok_or(MangleError::InvalidDockerfile)?
        + 1;

    // Copy swiftide-docker-service, rg, and fd into the image
    let copy_lines = ["swiftide-docker-service", "rg", "fd"]
        .iter()
        .map(|binary| format!("COPY --from={image_name} /usr/bin/{binary} /usr/bin/{binary}"))
        .collect::<Vec<String>>()
        .join("\n");

    lines.insert(insert_pos, &copy_lines);

    // If the last FROM line is alpine, add gcompat and libgcc
    if let Some(last_from) = lines
        .iter()
        .rfind(|line| line.trim_start().to_lowercase().starts_with("from"))
        && last_from.to_lowercase().contains("alpine")
    {
        lines.insert(
            insert_pos.saturating_add(1),
            "RUN apk add --no-cache gcompat libgcc pcre2 ripgrep fd",
        );
    }

    let new_dockerfile = lines.join("\n");
    tracing::debug!(
        original = content,
        mangled = new_dockerfile,
        "Mangled dockerfile"
    );
    Ok(MangledDockerfile {
        content: new_dockerfile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    // Normalizes the the dockerfile versions for consistent tests
    macro_rules! assert_snapshot {
        ($content:expr) => {{
            insta::with_settings!({filters => vec![
                (r"\b\d{1,2}\.\d{1,2}\.\d{1,2}", "[CARGO_PKG_VERSION]")
            ]}, {
                insta::assert_snapshot!($content);
            });
        }}
    }

    #[tokio::test]
    async fn test_mangle() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("Dockerfile");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "FROM alpine").unwrap();

        let result = mangle(&file_path).await.unwrap();

        assert!(
            result
                .content
                .contains("COPY --from=bosunai/swiftide-docker-service:"),
            "action {}",
            result.content
        );
        assert!(result.content.contains("/usr/bin/swiftide-docker-service"));
        assert!(result.content.contains("/usr/bin/rg"));
        assert!(result.content.contains("/usr/bin/fd"));
        assert_snapshot!(result.content)
    }

    #[tokio::test]
    async fn test_remove_cmd_entrypoint() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("Dockerfile");
        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            "FROM alpine\nCMD [\"echo\", \"Hello World\"]\nENTRYPOINT [\"/bin/sh\"]"
        )
        .unwrap();

        let result = mangle(&file_path).await.unwrap();

        assert!(
            !result.content.contains("CMD [\"echo\", \"Hello World\"]"),
            "actual {}",
            result.content
        );
        assert!(!result.content.contains("ENTRYPOINT [\"/bin/sh\"]"));
        assert_snapshot!(result.content)
    }

    #[tokio::test]
    async fn test_remove_cmd_entrypoint_case_insensitive() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("Dockerfile");
        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            "from alpine\ncmd [\"echo\", \"Hello World\"]\neNtryPoint [\"/bin/sh\"]"
        )
        .unwrap();

        let result = mangle(&file_path).await.unwrap();

        assert!(!result.content.contains("CMD [\"echo\", \"Hello World\"]"));
        assert!(!result.content.contains("ENTRYPOINT [\"/bin/sh\"]"));
        assert_snapshot!(result.content)
    }

    #[tokio::test]
    async fn test_mangle_with_multiple_forms_and_other_lines() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("Dockerfile");
        let mut file = File::create(&file_path).unwrap();
        writeln!(
            file,
            "FROM alpine\nFROM ubuntu\nRUN echo hello world\nCMD [\"true\"]"
        )
        .unwrap();

        let result = mangle(&file_path).await.unwrap();

        assert_snapshot!(result.content)
    }
}
