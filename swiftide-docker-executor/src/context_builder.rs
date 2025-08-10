use std::{
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
// use ignore::{overrides::OverrideBuilder, WalkBuilder};
use tokio::io::AsyncReadExt as _;
use tokio_tar::{Builder, EntryType, Header};
use walkdir::{DirEntry, WalkDir};

use crate::ContextError;

type ContextArchive = Vec<u8>;

#[derive(Debug)]
pub struct ContextBuilder {
    context_path: PathBuf,
    ignore: Gitignore,
    dockerfile: PathBuf,
    global: Option<Gitignore>,
}

impl ContextBuilder {
    pub fn from_path(
        context_path: impl Into<PathBuf>,
        dockerfile: impl AsRef<Path>,
    ) -> Result<Self, ContextError> {
        let path = context_path.into();
        let mut gitignore = GitignoreBuilder::new(&path);

        if let Some(err) = gitignore.add(path.join(".gitignore")) {
            tracing::warn!(?err, "Error adding .gitignore");
        }
        if let Some(err) = gitignore.add(path.join(".dockerignore")) {
            tracing::warn!(?err, "Error adding .dockerignore");
        }

        let gitignore = gitignore.build()?;

        let (global_gitignore, maybe_error) = Gitignore::global();
        let maybe_global = if let Some(err) = maybe_error {
            tracing::warn!(?err, "Error adding global gitignore");
            None
        } else {
            Some(global_gitignore)
        };

        Ok(Self {
            dockerfile: dockerfile.as_ref().to_path_buf(),
            context_path: path,
            ignore: gitignore,
            global: maybe_global,
        })
    }

    fn is_ignored(&self, path: impl AsRef<Path>) -> bool {
        let Ok(relative_path) = path.as_ref().strip_prefix(&self.context_path) else {
            tracing::debug!(
                "not ignoring {path} as it seems to be not prefixed by {prefix}",
                path = path.as_ref().display(),
                prefix = self.context_path.to_string_lossy()
            );
            return false;
        };

        if relative_path.starts_with(".git") {
            tracing::debug!(
                "not ignoring {path} as it seems to be a git file",
                path = path.as_ref().display()
            );
            return false;
        }

        if let Some(global) = &self.global
            && global
                .matched_path_or_any_parents(relative_path, false)
                .is_ignore()
        {
            tracing::debug!(
                "ignoring {path} as it is ignored by global gitignore",
                path = path.as_ref().display()
            );
            return true;
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

        // First lets add the actual dockerfile
        let mut file = fs_err::tokio::File::open(&self.dockerfile).await?;
        let mut buffer_content = Vec::new();
        file.read_to_end(&mut buffer_content).await?;

        // Prepare header for Dockerfile
        let mut header = Header::new_gnu();
        header.set_size(buffer_content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();

        // Add Dockerfile to tar
        tar.append_data(
            &mut header,
            self.dockerfile
                .file_name()
                .expect("Infallible; No file name"),
            &*buffer_content,
        )
        .await?;

        for entry in self.iter() {
            let Ok(entry) = entry else {
                tracing::warn!(?entry, "Failed to read entry");
                continue;
            };
            let path = entry.path();

            let Ok(relative_path) = path.strip_prefix(&self.context_path) else {
                tracing::warn!(?path, "Failed to strip prefix on path");
                continue;
            };

            if path.is_dir() && !path.is_symlink() {
                tracing::debug!(path = ?path, relative_path = ?relative_path, "Adding directory to tar");
                if let Err(err) = tar.append_path(relative_path).await {
                    tracing::warn!(?err, "Failed to append path to tar");
                }
                continue;
            }

            if self.is_ignored(path) {
                tracing::debug!(path = ?path, "Ignored file");
                continue;
            }

            if path.is_symlink() {
                tracing::debug!(path = ?path, "Adding symlink to tar");
                let Ok(link_target) = tokio::fs::read_link(path).await else {
                    continue;
                }; // The target of the symlink
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                tracing::debug!(link_target = ?link_target, "Symlink target");
                let mut header = Header::new_gnu();

                // Indicate it's a symlink
                header.set_entry_type(EntryType::Symlink);
                // The tar specification requires setting the link name for a symlink
                if let Err(error) = header.set_link_name(&link_target) {
                    tracing::warn!(?error, "Failed to set link name on {link_target:#?}");
                    continue;
                }

                // Set ownership, permissions, etc.
                header.set_uid(metadata.uid() as u64);
                header.set_gid(metadata.gid() as u64);
                // For a symlink, the "mode" is often ignored by many tools,
                // but we’ll set it anyway to match the source:
                header.set_mode(metadata.mode());
                // Set modification time (use 0 or a real timestamp as you prefer)
                header.set_mtime(metadata.mtime() as u64);
                // Symlinks don’t store file data in the tar, so size is 0
                header.set_size(0);

                if let Err(error) = tar.append_data(&mut header, path, tokio::io::empty()).await {
                    tracing::warn!(
                        ?error,
                        "Failed to append symlink to tar on {link_target:#?}"
                    );
                    continue;
                }
                continue;
            }

            tracing::debug!(path = ?path, "Adding file to tar");
            let mut file = fs_err::tokio::File::open(path).await?;
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
    use tempfile::{NamedTempFile, tempdir};

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

        let dockerfile = NamedTempFile::new().unwrap();

        let context_builder = ContextBuilder::from_path(&context_path, dockerfile.path()).unwrap();

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

        let dockerfile = NamedTempFile::new().unwrap();
        let context_builder = ContextBuilder::from_path(&context_path, dockerfile.path()).unwrap();

        assert!(!context_builder.is_ignored(".git"));
    }

    #[test_log::test(tokio::test)]
    async fn test_works_without_gitignore() {
        let dir = tempdir().unwrap();
        let context_path = dir.path().to_path_buf();

        // Create .gitignore file

        let dockerfile = NamedTempFile::new().unwrap();

        let context_builder = ContextBuilder::from_path(&context_path, dockerfile.path()).unwrap();

        assert!(!context_builder.is_ignored(".git"));
        assert!(!context_builder.is_ignored("Dockerfile"));

        fs::File::create(context_path.join("Dockerfile")).unwrap();

        assert!(!context_builder.is_ignored("Dockerfile"));
    }
}
