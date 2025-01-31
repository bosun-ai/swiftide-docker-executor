/// Adds copy statements to the Dockerfile to copy the built binary into the image.
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::fs::read_to_string;

use crate::MangleError;

pub struct MangledDockerfile {
    pub original_path: PathBuf,
    pub content: String,
}

pub async fn mangle(path: &Path) -> Result<MangledDockerfile, MangleError> {
    let this_crate = env!("CARGO_PKG_VERSION");
    let mut content = read_to_string(path)
        .await
        .map_err(MangleError::DockerfileReadError)?;

    let image_name = format!("bosun-ai/swiftide-docker-service:{this_crate}");

    // copy swiftide-docker-serivice, rg, and fd into the image
    let copy_lines = ["swiftide-docker-service", "rg", "fd"]
        .iter()
        .map(
            |binary| format!("COPY --from={image_name} /usr/bin/{binary} /usr/local/bin/{binary}",),
        )
        .collect::<Vec<String>>()
        .join("\n");

    content.push_str(&copy_lines);

    Ok(MangledDockerfile {
        content,
        original_path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;
    use tokio::runtime::Runtime;

    #[test]
    fn test_mangle() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("Dockerfile");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "FROM alpine").unwrap();

        let rt = Runtime::new().unwrap();
        let result = rt.block_on(mangle(&file_path)).unwrap();

        assert!(result
            .content
            .contains("COPY --from=bosun-ai/swiftide-docker-service:"));
        assert!(result
            .content
            .contains("/usr/local/bin/swiftide-docker-service"));
        assert!(result.content.contains("/usr/local/bin/rg"));
        assert!(result.content.contains("/usr/local/bin/fd"));
    }
}
