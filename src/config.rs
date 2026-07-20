use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default model to use when no config or CLI flag is specified
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Path to the claude-print config file
const CONFIG_PATH: &str = ".claude/claude-print.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub defaults: Option<Defaults>,
}

impl Config {
    /// Returns the path to the config file in the user's home directory
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var("HOME")
            .map_err(|_| Error::Config("HOME environment variable not set".to_string()))?;
        Ok(PathBuf::from(home).join(CONFIG_PATH))
    }

    /// Loads the config file, returning an empty config if the file doesn't exist
    pub fn load_or_default(path: &PathBuf) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
                eprintln!(
                    "claude-print: warning: invalid config at {}: {}",
                    path.display(),
                    e
                );
                Config { defaults: None }
            }),
            Err(_) => Config { defaults: None },
        }
    }

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

    /// Resolves the model to use, with priority:
    /// 1. CLI flag (passed in)
    /// 2. Config file default
    /// 3. Hardcoded default
    pub fn resolve_model(&self, cli_model: Option<String>) -> String {
        cli_model
            .or(self.default_model().map(|s| s.to_string()))
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
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

    #[test]
    fn resolve_model_cli_flag_overrides_config() {
        let config = Config {
            defaults: Some(Defaults {
                model: Some("claude-opus-4-8".to_string()),
            }),
        };
        assert_eq!(
            config.resolve_model(Some("claude-haiku-4-5".to_string())),
            "claude-haiku-4-5"
        );
    }

    #[test]
    fn resolve_model_config_when_no_cli_flag() {
        let config = Config {
            defaults: Some(Defaults {
                model: Some("claude-opus-4-8".to_string()),
            }),
        };
        assert_eq!(config.resolve_model(None), "claude-opus-4-8");
    }

    #[test]
    fn resolve_model_defaults_when_no_config_or_cli() {
        let config = Config { defaults: None };
        assert_eq!(config.resolve_model(None), DEFAULT_MODEL);
    }

    #[test]
    fn resolve_model_cli_overrides_no_config() {
        let config = Config { defaults: None };
        assert_eq!(
            config.resolve_model(Some("custom-model".to_string())),
            "custom-model"
        );
    }

    #[test]
    fn load_or_default_returns_defaults_when_file_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_path = temp_dir.path().join("missing-config.toml");
        let config = Config::load_or_default(&missing_path);
        assert!(config.defaults.is_none());
    }

    #[test]
    fn load_or_default_parses_valid_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4-8""#,
        )
        .unwrap();

        let config = Config::load_or_default(&config_path);
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
    }

    #[test]
    fn load_or_default_returns_defaults_on_invalid_toml() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("invalid-config.toml");
        std::fs::write(&config_path, "invalid toml content [[").unwrap();

        let config = Config::load_or_default(&config_path);
        assert!(config.defaults.is_none());
    }

    #[test]
    fn load_fails_on_missing_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_path = temp_dir.path().join("missing-config.toml");
        assert!(Config::load(&missing_path).is_err());
    }

    #[test]
    fn load_fails_on_invalid_toml() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("invalid-config.toml");
        std::fs::write(&config_path, "invalid toml content [[").unwrap();

        assert!(Config::load(&config_path).is_err());
    }
}
