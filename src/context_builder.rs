use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
// use ignore::{overrides::OverrideBuilder, WalkBuilder};
use tokio::io::AsyncReadExt as _;
use tokio_tar::{Builder, Header};
use walkdir::{DirEntry, WalkDir};

use crate::ContextError;

type ContextArchive = Vec<u8>;

#[derive(Debug)]
pub struct ContextBuilder {
    context_path: PathBuf,
    ignore: Gitignore,
}

impl ContextBuilder {
    pub fn from_path(context_path: impl Into<PathBuf>) -> Result<Self, ContextError> {
        let path = context_path.into();
        let mut gitignore = GitignoreBuilder::new(&path);

        if let Some(err) = gitignore.add(path.join(".gitignore")) {
            tracing::warn!(?err, "Error adding .gitignore");
        }
        if let Some(err) = gitignore.add(path.join(".dockerignore")) {
            tracing::warn!(?err, "Error adding .dockerignore");
        }

        let (gitignore, maybe_error) = gitignore.build_global();

        if let Some(err) = maybe_error {
            return Err(ContextError::FailedIgnore(err));
        }

        Ok(Self {
            context_path: path,
            ignore: gitignore,
        })
    }

    fn is_ignored(&self, path: impl AsRef<Path>) -> bool {
        let Ok(relative_path) = path.as_ref().strip_prefix(&self.context_path) else {
            return false;
        };

        if relative_path.starts_with(".git") {
            return false;
        }

        self.ignore
            .matched_path_or_any_parents(relative_path, false)
            .is_ignore()
    }

    fn iter(&self) -> impl Iterator<Item = Result<DirEntry, walkdir::Error>> {
        WalkDir::new(&self.context_path).into_iter()
    }

    pub async fn build_tar(&self) -> Result<ContextArchive, ContextError> {
        let buffer = Vec::new();

        let mut tar = Builder::new(buffer);

        for entry in self.iter() {
            let Ok(entry) = entry else { continue };
            let path = entry.path();

            if !path.is_file() {
                tracing::debug!(path = ?path, "Ignore non-file");
                continue;
            }
            if self.is_ignored(path) {
                tracing::debug!(path = ?path, "Ignored file");
                continue;
            }

            tracing::debug!(path = ?path, "Adding file to tar");
            let mut file = tokio::fs::File::open(path).await?;
            let mut buffer_content = Vec::new();
            file.read_to_end(&mut buffer_content).await?;

            let mut header = Header::new_gnu();
            header.set_size(buffer_content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();

            let relative_path = path.strip_prefix(&self.context_path)?;
            tar.append_data(&mut header, relative_path, &*buffer_content)
                .await?;
        }

        let result = tar.into_inner().await?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    #[test_log::test(tokio::test)]
    async fn test_is_ignored() {
        let dir = tempdir().unwrap();
        let context_path = dir.path().to_path_buf();

        // Create .gitignore file
        let mut gitignore_file = fs::File::create(context_path.join(".gitignore")).unwrap();
        writeln!(gitignore_file, "*.log").unwrap();

        // Create .dockerignore file
        let mut dockerignore_file = fs::File::create(context_path.join(".dockerignore")).unwrap();
        writeln!(dockerignore_file, "*.tmp").unwrap();

        dbg!(&std::fs::read_to_string(context_path.join(".gitignore")).unwrap());

        let context_builder = ContextBuilder::from_path(&context_path).unwrap();

        // Create test files
        let log_file = context_path.join("test.log");
        let tmp_file = context_path.join("test.tmp");
        let txt_file = context_path.join("test.txt");

        fs::File::create(&log_file).unwrap();
        fs::File::create(&tmp_file).unwrap();
        fs::File::create(&txt_file).unwrap();

        assert!(context_builder.is_ignored(&log_file));
        assert!(context_builder.is_ignored(&tmp_file));
        assert!(!context_builder.is_ignored(&txt_file));
    }

    #[test_log::test(tokio::test)]
    async fn test_adds_git_even_if_in_ignore() {
        let dir = tempdir().unwrap();
        let context_path = dir.path().to_path_buf();

        // Create .gitignore file
        let mut gitignore_file = fs::File::create(context_path.join(".gitignore")).unwrap();
        writeln!(gitignore_file, ".git").unwrap();

        let context_builder = ContextBuilder::from_path(&context_path).unwrap();

        assert!(!context_builder.is_ignored(".git"));
    }

    #[test_log::test(tokio::test)]
    async fn test_works_without_gitignore() {
        let dir = tempdir().unwrap();
        let context_path = dir.path().to_path_buf();

        // Create .gitignore file

        let context_builder = ContextBuilder::from_path(&context_path).unwrap();

        assert!(!context_builder.is_ignored(".git"));
        assert!(!context_builder.is_ignored("Dockerfile"));

        fs::File::create(context_path.join("Dockerfile")).unwrap();

        assert!(!context_builder.is_ignored("Dockerfile"));
    }
}
