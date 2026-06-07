use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub defaults: Option<Defaults>,
}

impl Config {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        let config: Config = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("invalid config at {}: {e}", path.display())))?;
        Ok(config)
    }

    pub fn default_model(&self) -> Option<&str> {
        self.defaults.as_ref()?.model.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_none_when_no_defaults() {
        let config = Config { defaults: None };
        assert!(config.default_model().is_none());
    }

    #[test]
    fn default_model_returns_value() {
        let config = Config {
            defaults: Some(Defaults {
                model: Some("claude-opus-4-8".to_string()),
            }),
        };
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
    }
}
