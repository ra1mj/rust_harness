//! MCP server hub: persist server configs and bridge their tools into the
//! harness's tool registry.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Result};

use rh_core::{Context, Disposer};
use rh_mcp::{connect, McpClient, McpServerConfig};
use rh_tool::ToolRegistry;

struct ConnectedServer {
    config: McpServerConfig,
    // Held for the server's lifetime: dropping kills the child (client) and
    // unregisters its tools (disposers).
    _client: McpClient,
    _disposers: Vec<Disposer>,
}

pub struct McpHub {
    servers: RwLock<Vec<McpServerConfig>>,
    connected: Mutex<Vec<ConnectedServer>>,
    path: PathBuf,
}

impl McpHub {
    pub fn load(path: PathBuf) -> Arc<Self> {
        let servers = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<McpServerConfig>>(&s).ok())
            .unwrap_or_default();
        Arc::new(Self {
            servers: RwLock::new(servers),
            connected: Mutex::new(Vec::new()),
            path,
        })
    }

    fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&*self.servers.read().unwrap())?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        std::fs::write(&self.path, json)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<McpServerConfig> {
        self.servers.read().unwrap().clone()
    }

    /// Connect all configured servers and register their tools.
    pub async fn connect_all(&self, ctx: &Context) {
        let registry = match ctx.service::<ToolRegistry>() {
            Some(r) => r,
            None => return,
        };
        let configs: Vec<McpServerConfig> = self.servers.read().unwrap().clone();
        for config in configs {
            let _ = self.connect_one(&registry, config).await;
        }
    }

    async fn connect_one(&self, registry: &ToolRegistry, config: McpServerConfig) -> Result<()> {
        if self
            .connected
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.config.name == config.name)
        {
            return Ok(());
        }
        let (client, tools) = connect(&config).await?;
        let mut disposers = Vec::new();
        for tool in tools {
            disposers.push(registry.register(tool));
        }
        self.connected.lock().unwrap().push(ConnectedServer {
            config,
            _client: client,
            _disposers: disposers,
        });
        Ok(())
    }

    fn disconnect(&self, name: &str) {
        self.connected
            .lock()
            .unwrap()
            .retain(|c| c.config.name != name);
    }

    pub async fn add(&self, ctx: &Context, config: McpServerConfig) -> Result<()> {
        {
            let mut servers = self.servers.write().unwrap();
            match servers.iter_mut().find(|s| s.name == config.name) {
                Some(existing) => *existing = config.clone(),
                None => servers.push(config.clone()),
            }
        }
        self.save()?;
        self.disconnect(&config.name);
        let registry = ctx
            .service::<ToolRegistry>()
            .ok_or_else(|| anyhow!("no tool registry registered"))?;
        self.connect_one(&registry, config).await
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        {
            let mut servers = self.servers.write().unwrap();
            servers.retain(|s| s.name != name);
        }
        self.save()?;
        self.disconnect(name);
        Ok(())
    }
}
