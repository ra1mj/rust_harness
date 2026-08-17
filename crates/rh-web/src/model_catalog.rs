//! The model catalog: user-managed models persisted to a JSON file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use rh_agent::{MockModelProvider, ModelConfig, ModelProvider};

/// On-disk shape: the active model id plus the model list.
#[derive(Serialize, Deserialize, Default)]
struct CatalogFile {
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    models: Vec<ModelConfig>,
}

/// A thread-safe, persisted collection of models plus the active selection.
pub struct ModelCatalog {
    inner: RwLock<CatalogFile>,
    path: PathBuf,
    next_id: AtomicU64,
}

impl ModelCatalog {
    /// Load (or seed) the catalog from `path`.
    pub fn load(path: PathBuf) -> Self {
        let mut file = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<CatalogFile>(&s).ok())
            .unwrap_or_default();

        if !file.models.iter().any(|m| m.provider == "mock") {
            file.models.insert(0, ModelConfig::mock());
        }
        if std::env::var("RH_API_KEY").is_ok() && !file.models.iter().any(|m| m.provider == "openai")
        {
            file.models.push(env_model());
        }
        if file.active.is_none() {
            file.active = file.models.first().map(|m| m.id.clone());
        }

        let catalog = Self {
            inner: RwLock::new(file),
            path,
            next_id: AtomicU64::new(0),
        };
        let _ = catalog.save();
        catalog
    }

    pub fn list(&self) -> Vec<ModelConfig> {
        self.inner.read().expect("catalog poisoned").models.clone()
    }

    pub fn active_id(&self) -> Option<String> {
        self.inner.read().expect("catalog poisoned").active.clone()
    }

    /// The currently-selected model config.
    pub fn active(&self) -> Option<ModelConfig> {
        let inner = self.inner.read().expect("catalog poisoned");
        let id = inner.active.as_ref()?;
        inner.models.iter().find(|m| &m.id == id).cloned()
    }

    pub fn set_active(&self, id: &str) -> Result<()> {
        {
            let mut inner = self.inner.write().expect("catalog poisoned");
            if !inner.models.iter().any(|m| m.id == id) {
                return Err(anyhow!("model {id} not found"));
            }
            inner.active = Some(id.to_string());
        }
        self.save()
    }

    /// Insert or replace a model; generates an id when empty.
    pub fn upsert(&self, mut config: ModelConfig) -> Result<ModelConfig> {
        if config.id.trim().is_empty() {
            config.id = format!("m-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        }
        {
            let mut inner = self.inner.write().expect("catalog poisoned");
            match inner.models.iter_mut().find(|m| m.id == config.id) {
                Some(existing) => *existing = config.clone(),
                None => inner.models.push(config.clone()),
            }
            if inner.active.is_none() {
                inner.active = Some(config.id.clone());
            }
        }
        self.save()?;
        Ok(config)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        if id == "mock" {
            return Err(anyhow!("内置的 mock 模型不可删除"));
        }
        {
            let mut inner = self.inner.write().expect("catalog poisoned");
            inner.models.retain(|m| m.id != id);
            if inner.active.as_deref() == Some(id) {
                inner.active = inner.models.first().map(|m| m.id.clone());
            }
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&*self.inner.read().expect("catalog poisoned"))?;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        std::fs::write(&self.path, json)?;
        Ok(())
    }
}

fn env_model() -> ModelConfig {
    ModelConfig {
        id: "deepseek".to_string(),
        label: "DeepSeek".to_string(),
        provider: "openai".to_string(),
        base_url: Some(
            std::env::var("RH_BASE_URL").unwrap_or_else(|_| "https://api.deepseek.com".to_string()),
        ),
        api_key: std::env::var("RH_API_KEY").ok(),
        model: Some(std::env::var("RH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string())),
    }
}

/// Build a model provider from a config.
pub fn build_provider(config: &ModelConfig) -> Result<Arc<dyn ModelProvider>> {
    match config.provider.as_str() {
        "mock" => Ok(Arc::new(MockModelProvider {
            model: config.model.clone().unwrap_or_else(|| "mock".to_string()),
        })),
        "openai" => {
            let base_url = config
                .base_url
                .clone()
                .ok_or_else(|| anyhow!("base_url 必填"))?;
            let api_key = config
                .api_key
                .clone()
                .ok_or_else(|| anyhow!("api_key 必填"))?;
            let model = config
                .model
                .clone()
                .ok_or_else(|| anyhow!("model 必填"))?;
            Ok(Arc::new(rh_providers::OpenAiCompatibleProvider::new(
                base_url, api_key, model,
            )))
        }
        other => Err(anyhow!("未知的 provider：{other}")),
    }
}
