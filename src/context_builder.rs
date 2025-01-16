use std::path::Path;

use ignore::{overrides::OverrideBuilder, WalkBuilder};
use tokio::io::AsyncReadExt as _;
use tokio_tar::{Builder, Header};

use crate::ContextError;

type ContextArchive = Vec<u8>;

pub struct ContextBuilder {}

impl ContextBuilder {
    pub async fn build_from_path(context_path: &Path) -> Result<ContextArchive, ContextError> {
        let buffer = Vec::new();

        let mut tar = Builder::new(buffer);

        // Ensure we *do* include the .git directory
        let overrides = OverrideBuilder::new(context_path).add(".git")?.build()?;

        for entry in WalkBuilder::new(context_path)
            // .overrides(overrides)
            .hidden(false)
            .add_custom_ignore_filename(".dockerignore")
            .overrides(overrides)
            .build()
        {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let mut file = tokio::fs::File::open(path).await?;
                let mut buffer_content = Vec::new();
                file.read_to_end(&mut buffer_content).await?;

                let mut header = Header::new_gnu();
                header.set_size(buffer_content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();

                let relative_path = path.strip_prefix(context_path)?;
                tar.append_data(&mut header, relative_path, &*buffer_content)
                    .await?;
            }
        }

        let result = tar.into_inner().await?;

        Ok(result.clone())
    }
}
