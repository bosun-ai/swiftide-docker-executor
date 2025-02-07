use std::io::Write;
use std::path::Path;

use crate::dockerfile_mangler::mangle;
use crate::DockerfileError;

pub struct DockerfileManager {
    context_path: std::path::PathBuf,
}

impl DockerfileManager {
    pub fn new(context_path: &Path) -> Self {
        Self {
            context_path: context_path.to_path_buf(),
        }
    }

    pub async fn prepare_dockerfile(
        &self,
        dockerfile: &Path,
    ) -> Result<tempfile::NamedTempFile, DockerfileError> {
        let valid_dockerfile_path = if dockerfile.is_relative() {
            self.context_path.join(dockerfile)
            // dockerfile.to_path_buf()
        } else {
            // self.context_path.join(dockerfile)
            dockerfile.to_path_buf()
        };

        let mangled_dockerfile = mangle(&valid_dockerfile_path).await?;

        let mut tmp_dockerfile = tempfile::NamedTempFile::new_in(&self.context_path)
            .map_err(DockerfileError::TempFileError)?;

        tmp_dockerfile
            .write_all(mangled_dockerfile.content.as_bytes())
            .map_err(DockerfileError::TempFileError)?;

        tmp_dockerfile
            .flush()
            .map_err(DockerfileError::TempFileError)?;

        tracing::debug!(
            "Created temporary dockerfile at {}",
            tmp_dockerfile.path().display()
        );

        Ok(tmp_dockerfile)
    }
}
