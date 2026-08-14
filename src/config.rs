use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default model to use when no config or CLI flag is specified
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Config file basename
const CONFIG_FILENAME: &str = "config.toml";

/// Config directory name under XDG_CONFIG_HOME or ~/.config
const CONFIG_DIR: &str = "claude-print";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Whether to inherit user hooks (default: true)
    pub inherit_hooks: Option<bool>,
    /// Default model to use
    pub model: Option<String>,
    /// Maximum number of turns (default: 30)
    pub max_turns: Option<u32>,
    /// Timeout in seconds (default: 3600)
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub defaults: Option<Defaults>,
}

impl Config {
    /// Returns the path to the config file
    ///
    /// Path priority:
    /// 1. $XDG_CONFIG_HOME/claude-print/config.toml if XDG_CONFIG_HOME is set
    /// 2. ~/.config/claude-print/config.toml otherwise
    pub fn default_path() -> Result<PathBuf> {
        // Try XDG_CONFIG_HOME first
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg_config)
                .join(CONFIG_DIR)
                .join(CONFIG_FILENAME));
        }

        // Fall back to ~/.config
        let home = std::env::var("HOME")
            .map_err(|_| Error::Config("HOME environment variable not set".to_string()))?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join(CONFIG_DIR)
            .join(CONFIG_FILENAME))
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

    /// Resolves the inherit_hooks flag, with priority:
    /// 1. CLI flag (passed in)
    /// 2. Config file default
    /// 3. Hardcoded default (true)
    pub fn resolve_inherit_hooks(&self, cli_value: Option<bool>) -> bool {
        cli_value.or(self.default_inherit_hooks()).unwrap_or(true)
    }

    /// Resolves max_turns, with priority:
    /// 1. CLI flag (passed in)
    /// 2. Config file default
    /// 3. Hardcoded default (30)
    pub fn resolve_max_turns(&self, cli_value: Option<u32>) -> u32 {
        cli_value.or(self.default_max_turns()).unwrap_or(30)
    }

    /// Resolves timeout_secs, with priority:
    /// 1. CLI flag (passed in)
    /// 2. Config file default
    /// 3. Hardcoded default (3600)
    pub fn resolve_timeout_secs(&self, cli_value: Option<u64>) -> u64 {
        cli_value.or(self.default_timeout_secs()).unwrap_or(3600)
    }

    fn default_inherit_hooks(&self) -> Option<bool> {
        self.defaults.as_ref()?.inherit_hooks
    }

    fn default_max_turns(&self) -> Option<u32> {
        self.defaults.as_ref()?.max_turns
    }

    fn default_timeout_secs(&self) -> Option<u64> {
        self.defaults.as_ref()?.timeout_secs
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
                inherit_hooks: None,
                model: Some("claude-opus-4-8".to_string()),
                max_turns: None,
                timeout_secs: None,
            }),
        };
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
    }

    #[test]
    fn resolve_model_cli_flag_overrides_config() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: None,
                model: Some("claude-opus-4-8".to_string()),
                max_turns: None,
                timeout_secs: None,
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
                inherit_hooks: None,
                model: Some("claude-opus-4-8".to_string()),
                max_turns: None,
                timeout_secs: None,
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

    #[test]
    fn default_path_uses_xdg_config_home_when_set() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        let path = Config::default_path().unwrap();
        assert_eq!(
            path,
            temp_dir.path().join("claude-print").join("config.toml")
        );
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn default_path_fallback_to_home_config_when_xdg_not_set() {
        std::env::remove_var("XDG_CONFIG_HOME");
        let path = Config::default_path().unwrap();
        // Should be ~/.config/claude-print/config.toml
        let expected = std::env::var("HOME")
            .map(|h| {
                PathBuf::from(h)
                    .join(".config")
                    .join("claude-print")
                    .join("config.toml")
            })
            .unwrap();
        assert_eq!(path, expected);
    }

    #[test]
    fn default_inherit_hooks_none_when_no_defaults() {
        let config = Config { defaults: None };
        assert!(config.default_inherit_hooks().is_none());
    }

    #[test]
    fn resolve_inherit_hooks_cli_overrides_config() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: Some(false),
                model: Some("claude-opus-4-8".to_string()),
                max_turns: None,
                timeout_secs: None,
            }),
        };
        assert!(config.resolve_inherit_hooks(Some(true)));
    }

    #[test]
    fn resolve_inherit_hooks_config_when_no_cli() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: Some(false),
                model: None,
                max_turns: None,
                timeout_secs: None,
            }),
        };
        assert!(!config.resolve_inherit_hooks(None));
    }

    #[test]
    fn resolve_inherit_hooks_defaults_to_true() {
        let config = Config { defaults: None };
        assert_eq!(config.resolve_inherit_hooks(None), true);
    }

    #[test]
    fn default_max_turns_none_when_no_defaults() {
        let config = Config { defaults: None };
        assert!(config.default_max_turns().is_none());
    }

    #[test]
    fn resolve_max_turns_cli_overrides_config() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: None,
                model: None,
                max_turns: Some(20),
                timeout_secs: None,
            }),
        };
        assert_eq!(config.resolve_max_turns(Some(50)), 50);
    }

    #[test]
    fn resolve_max_turns_config_when_no_cli() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: None,
                model: None,
                max_turns: Some(20),
                timeout_secs: None,
            }),
        };
        assert_eq!(config.resolve_max_turns(None), 20);
    }

    #[test]
    fn resolve_max_turns_defaults_to_30() {
        let config = Config { defaults: None };
        assert_eq!(config.resolve_max_turns(None), 30);
    }

    #[test]
    fn default_timeout_secs_none_when_no_defaults() {
        let config = Config { defaults: None };
        assert!(config.default_timeout_secs().is_none());
    }

    #[test]
    fn resolve_timeout_secs_cli_overrides_config() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: None,
                model: None,
                max_turns: None,
                timeout_secs: Some(1800),
            }),
        };
        assert_eq!(config.resolve_timeout_secs(Some(7200)), 7200);
    }

    #[test]
    fn resolve_timeout_secs_config_when_no_cli() {
        let config = Config {
            defaults: Some(Defaults {
                inherit_hooks: None,
                model: None,
                max_turns: None,
                timeout_secs: Some(1800),
            }),
        };
        assert_eq!(config.resolve_timeout_secs(None), 1800);
    }

    #[test]
    fn resolve_timeout_secs_defaults_to_3600() {
        let config = Config { defaults: None };
        assert_eq!(config.resolve_timeout_secs(None), 3600);
    }

    #[test]
    fn load_or_default_rejects_unknown_fields() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4-8"
unknown_field = "should_fail""#,
        )
        .unwrap();

        let config = Config::load_or_default(&config_path);
        // Due to deny_unknown_fields, this should return empty config on parse error
        assert!(config.defaults.is_none());
    }

    #[test]
    fn load_or_default_parses_all_defaults() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
inherit_hooks = false
model = "claude-opus-4-8"
max_turns = 50
timeout_secs = 1800"#,
        )
        .unwrap();

        let config = Config::load_or_default(&config_path);
        assert_eq!(config.default_inherit_hooks(), Some(false));
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
        assert_eq!(config.default_max_turns(), Some(50));
        assert_eq!(config.default_timeout_secs(), Some(1800));
    }
}
