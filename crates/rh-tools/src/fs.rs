//! The filesystem capability seam: definition, local provider, and plugin.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use rh_core::{Context, Disposers, Plugin};
use rh_tool::ToolError;

/// Service Definition: filesystem read/write.
#[async_trait]
pub trait FileSystem: Send + Sync {
    async fn read(&self, path: &Path) -> Result<String, ToolError>;
    async fn write(&self, path: &Path, content: &str) -> Result<(), ToolError>;
}

/// Service Provider: the local filesystem.
pub struct LocalFileSystem;

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn read(&self, path: &Path) -> Result<String, ToolError> {
        tokio::fs::read_to_string(path).await.map_err(|e| {
            ToolError::execution(format!("读取文件失败 {}：{e}", path.display()))
        })
    }

    async fn write(&self, path: &Path, content: &str) -> Result<(), ToolError> {
        tokio::fs::write(path, content).await.map_err(|e| {
            ToolError::execution(format!("写入文件失败 {}：{e}", path.display()))
        })
    }
}

/// Mounts the local filesystem provider.
pub struct FileSystemPlugin;

impl Plugin for FileSystemPlugin {
    fn name(&self) -> &'static str {
        "fs:local"
    }

    fn mount(&self, ctx: &Context) -> anyhow::Result<Disposers> {
        let fs: Arc<dyn FileSystem> = Arc::new(LocalFileSystem);
        Ok(vec![ctx.provide_named("FileSystem(local)", fs)])
    }
}
