//! Plugin command trait (port of `PostcardsDotnet.PluginBase.ICommand`).
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// High-level summary of a photo album in the backing service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlbumSummary {
    pub id: String,
    pub name: String,
    pub asset_count: usize,
}

/// A photo source plugin: login, sync, provide the next photo, delete / unlink it.
#[async_trait]
pub trait ICommand: Send + Sync {
    /// Command / plugin name.
    fn name(&self) -> &str;

    /// Command / plugin description.
    fn description(&self) -> &str;

    /// Login to the backing service.
    async fn login(&mut self) -> anyhow::Result<()>;

    /// Sync new photos into the local cache folder.
    async fn sync(&mut self) -> anyhow::Result<()>;

    /// Get the next photo to send (oldest first).
    async fn get_next_photo(&self) -> anyhow::Result<std::path::PathBuf>;

    /// Called after a postcard was successfully sent.
    /// Default implementation removes the local cached photo file.
    /// Backends like Immich override this to also remove the photo from the
    /// remote album so it is not sent again.
    async fn on_photo_sent(&mut self, path_to_photo: &std::path::Path) -> anyhow::Result<()> {
        self.delete_cached_photo(path_to_photo).await
    }

    /// Delete a specific cached photo locally.
    async fn delete_cached_photo(&self, path_to_photo: &std::path::Path) -> anyhow::Result<()>;

    /// List available albums in the backing service (optional support).
    async fn list_albums(&self) -> anyhow::Result<Vec<AlbumSummary>> {
        Ok(Vec::new())
    }
}
