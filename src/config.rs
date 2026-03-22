use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

const APP_NAME: &str = "mailman";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub gmail: GmailConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmailConfig {
    pub account_email: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub token_store: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gmail: GmailConfig {
                account_email: "you@example.com".to_string(),
                client_id: "replace-me".to_string(),
                client_secret: "replace-me".to_string(),
                redirect_url: "http://localhost:8080".to_string(),
                token_store: "tokens.json".to_string(),
            },
        }
    }
}

impl AppConfig {
    pub fn load_or_init() -> Result<Self> {
        let path = config_file_path()?;

        if !path.exists() {
            Self::init_default_config()?;
        }

        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file at {}", path.display()))?;

        toml::from_str(&raw)
            .with_context(|| format!("failed to parse config file at {}", path.display()))
    }

    pub fn init_default_config() -> Result<PathBuf> {
        let path = config_file_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;

        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;

        if !path.exists() {
            let contents =
                toml::to_string_pretty(&Self::default()).context("failed to serialize config")?;
            fs::write(&path, contents)
                .with_context(|| format!("failed to write config file at {}", path.display()))?;
        }

        Ok(path)
    }

    pub fn data_dir() -> Result<PathBuf> {
        let base = dirs::data_dir().ok_or_else(|| anyhow!("unable to resolve data directory"))?;
        Ok(base.join(APP_NAME))
    }

    pub fn token_store_path(&self) -> Result<PathBuf> {
        Ok(Self::data_dir()?.join(&self.gmail.token_store))
    }

    pub fn cache_dir() -> Result<PathBuf> {
        Ok(Self::data_dir()?.join("cache"))
    }
}

pub fn config_file_path() -> Result<PathBuf> {
    let base = dirs::config_dir().ok_or_else(|| anyhow!("unable to resolve config directory"))?;
    Ok(base.join(APP_NAME).join(CONFIG_FILE))
}
