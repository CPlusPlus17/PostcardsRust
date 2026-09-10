//! Plugin command trait (port of `PostcardsDotnet.PluginBase.ICommand`).
use async_trait::async_trait;

/// A photo source plugin: login, sync, provide the next photo, delete it.
#[async_trait]
pub trait ICommand: Send {
    /// Command name.
    fn name(&self) -> &str;

    /// Command description.
    fn description(&self) -> &str;

    /// Login to the backing service.
    async fn login(&mut self) -> anyhow::Result<()>;

    /// Sync new photos into the local cache folder.
    async fn sync(&mut self) -> anyhow::Result<()>;

    /// Get the next photo to send (oldest first).
    async fn get_next_photo(&self) -> anyhow::Result<std::path::PathBuf>;

    /// Delete a specific cached photo.
    async fn delete_cached_photo(&self, path_to_photo: &std::path::Path) -> anyhow::Result<()>;
}
