use crate::error::{Error, Result};
use crate::util::get_home;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default model to use when no config or CLI flag is specified
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

/// Config file basename
const CONFIG_FILENAME: &str = "config.toml";

/// Config directory name under XDG_CONFIG_HOME or ~/.config
const CONFIG_DIR: &str = "claude-print";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

impl Defaults {
    /// Validates all config fields, returning an error if any are invalid.
    pub fn validate(&self) -> Result<()> {
        // Validate model name format
        if let Some(model) = &self.model {
            self.validate_model(model)?;
        }

        // Validate max_turns range
        if let Some(max_turns) = self.max_turns {
            self.validate_max_turns(max_turns)?;
        }

        // Validate timeout_secs range
        if let Some(timeout_secs) = self.timeout_secs {
            self.validate_timeout_secs(timeout_secs)?;
        }

        Ok(())
    }

    /// Validates model name format.
    /// Model names should be non-empty and contain only alphanumeric characters, hyphens, and underscores.
    /// Model names must start with "claude-" or be a known alias.
    fn validate_model(&self, model: &str) -> Result<()> {
        if model.is_empty() {
            return Err(Error::Config("model name cannot be empty".to_string()));
        }

        if model.len() > 100 {
            return Err(Error::Config(format!(
                "model name '{}' is too long (max 100 characters)",
                model
            )));
        }

        // Model names should contain only valid characters: alphanumeric, hyphen, underscore, dot
        let valid_chars = model
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.');

        if !valid_chars {
            return Err(Error::Config(format!(
                "model name '{}' contains invalid characters (allowed: alphanumeric, '-', '_', '.')",
                model
            )));
        }

        // Model names must start with "claude-" to ensure proper Claude model identification
        if !model.starts_with("claude-") {
            return Err(Error::Config(format!(
                "model name '{}' must start with 'claude-'",
                model
            )));
        }

        Ok(())
    }

    /// Validates max_turns is within acceptable range [1, 1000].
    fn validate_max_turns(&self, max_turns: u32) -> Result<()> {
        const MIN_MAX_TURNS: u32 = 1;
        const MAX_MAX_TURNS: u32 = 1000;

        if max_turns < MIN_MAX_TURNS {
            return Err(Error::Config(format!(
                "max_turns value {} is invalid: must be at least {}",
                max_turns, MIN_MAX_TURNS
            )));
        }

        if max_turns > MAX_MAX_TURNS {
            return Err(Error::Config(format!(
                "max_turns value {} is invalid: must be at most {}",
                max_turns, MAX_MAX_TURNS
            )));
        }

        Ok(())
    }

    /// Validates timeout_secs is within acceptable range [1, 86400] (24 hours).
    fn validate_timeout_secs(&self, timeout_secs: u64) -> Result<()> {
        const MIN_TIMEOUT_SECS: u64 = 1;
        const MAX_TIMEOUT_SECS: u64 = 86400; // 24 hours

        if timeout_secs < MIN_TIMEOUT_SECS {
            return Err(Error::Config(format!(
                "timeout_secs value {} is invalid: must be at least {}",
                timeout_secs, MIN_TIMEOUT_SECS
            )));
        }

        if timeout_secs > MAX_TIMEOUT_SECS {
            return Err(Error::Config(format!(
                "timeout_secs value {} is invalid: must be at most {} (24 hours)",
                timeout_secs, MAX_TIMEOUT_SECS
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Config {
    pub defaults: Option<Defaults>,
}

impl Config {
    /// Returns the path to the config file.
    ///
    /// Path priority:
    /// 1. `$XDG_CONFIG_HOME/claude-print/config.toml` if `XDG_CONFIG_HOME` is set
    /// 2. `$HOME/.config/claude-print/config.toml` otherwise
    ///
    /// The fallback uses [`get_home`](crate::util::get_home); it never substitutes
    /// `/root`, the passwd database, or the current directory. This keeps config
    /// lookup consistent with transcript and trust-state paths. An explicit
    /// `XDG_CONFIG_HOME` is already a complete path, so direct library calls do
    /// not need `HOME` for this function in that case. The CLI still validates
    /// `HOME` as a process-wide prerequisite before dispatch.
    ///
    /// # Errors
    ///
    /// When `XDG_CONFIG_HOME` is unavailable, returns `Error::Config` if `HOME`
    /// is unset, empty, inaccessible, not a directory, or not writable. See
    /// [`get_home`](crate::util::get_home) for the canonical strict-policy
    /// rationale and exact error forms.
    pub fn default_path() -> Result<PathBuf> {
        // Try XDG_CONFIG_HOME first
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg_config)
                .join(CONFIG_DIR)
                .join(CONFIG_FILENAME));
        }

        // Fall back to ~/.config
        let home = get_home()?;
        Ok(home.join(".config").join(CONFIG_DIR).join(CONFIG_FILENAME))
    }

    /// Loads the config file, using defaults if the file doesn't exist
    ///
    /// Returns the loaded config if the file exists and is valid.
    /// Returns default config if the file doesn't exist (allows running without config).
    /// Returns an error if the file exists but cannot be read, parsed, or validated.
    pub fn load_or_default(path: &PathBuf) -> Result<Self> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Config file doesn't exist — use defaults
                return Ok(Config::default());
            }
            Err(e) => {
                return Err(Error::Config(format!(
                    "cannot read config at {}: {}",
                    path.display(),
                    e
                )));
            }
        };

        let config: Config = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("invalid config at {}: {e}", path.display())))?;

        // Validate the config after parsing
        if let Some(ref defaults) = config.defaults {
            defaults.validate().map_err(|e| {
                Error::Config(format!(
                    "config validation failed at {}: {}",
                    path.display(),
                    e
                ))
            })?;
        }

        Ok(config)
    }

    pub fn load(path: &PathBuf) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        let config: Config = toml::from_str(&contents)
            .map_err(|e| Error::Config(format!("invalid config at {}: {e}", path.display())))?;

        // Validate the config after parsing
        if let Some(ref defaults) = config.defaults {
            defaults.validate().map_err(|e| {
                Error::Config(format!(
                    "config validation failed at {}: {}",
                    path.display(),
                    e
                ))
            })?;
        }

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
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let guard = Self {
                key,
                previous: std::env::var_os(key),
            };
            std::env::set_var(key, value);
            guard
        }

        fn remove(key: &'static str) -> Self {
            let guard = Self {
                key,
                previous: std::env::var_os(key),
            };
            std::env::remove_var(key);
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

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
        let result = Config::load_or_default(&missing_path);
        assert!(
            result.is_ok(),
            "missing config file should return defaults, not error"
        );
        let config = result.unwrap();
        assert_eq!(config, Config::default());
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

        let config = Config::load_or_default(&config_path).unwrap();
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
    }

    #[test]
    fn load_or_default_errors_on_invalid_toml() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("invalid-config.toml");
        std::fs::write(&config_path, "invalid toml content [[").unwrap();

        let result = Config::load_or_default(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("invalid config"));
    }

    #[test]
    fn load_or_default_errors_on_io_error_not_found_excluded() {
        let temp_dir = tempfile::tempdir().unwrap();
        // Try to read from a directory (which will cause an IO error other than NotFound)
        let dir_path = temp_dir.path().join("config-dir");
        std::fs::create_dir(&dir_path).unwrap();

        let result = Config::load_or_default(&dir_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("cannot read config"));
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
        let _lock = env_lock();
        let temp_dir = tempfile::tempdir().unwrap();
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", temp_dir.path());
        let path = Config::default_path().unwrap();
        assert_eq!(
            path,
            temp_dir.path().join("claude-print").join("config.toml")
        );
    }

    #[test]
    fn default_path_fallback_to_home_config_when_xdg_not_set() {
        let _lock = env_lock();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvGuard::set("HOME", home.path());
        let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

        let path = Config::default_path().unwrap();
        // Should be ~/.config/claude-print/config.toml
        let expected = home
            .path()
            .join(".config")
            .join("claude-print")
            .join("config.toml");
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
        assert!(config.resolve_inherit_hooks(None));
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

        let result = Config::load_or_default(&config_path);
        // Due to deny_unknown_fields, this should error on parse
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("invalid config"));
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

        let config = Config::load_or_default(&config_path).unwrap();
        assert_eq!(config.default_inherit_hooks(), Some(false));
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
        assert_eq!(config.default_max_turns(), Some(50));
        assert_eq!(config.default_timeout_secs(), Some(1800));
    }

    #[test]
    fn load_rejects_max_turns_zero() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 0"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_turns"));
        assert!(err_msg.contains("at least 1"));
    }

    #[test]
    fn load_rejects_max_turns_too_large() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 1001"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_turns"));
        assert!(err_msg.contains("at most 1000"));
    }

    #[test]
    fn load_rejects_max_turns_absurdly_large() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 1000000"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_turns"));
        assert!(err_msg.contains("at most 1000"));
    }

    #[test]
    fn load_accepts_max_turns_at_lower_bound() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 1"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_max_turns(), Some(1));
    }

    #[test]
    fn load_accepts_max_turns_at_upper_bound() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 1000"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_max_turns(), Some(1000));
    }

    #[test]
    fn load_rejects_timeout_secs_zero() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = 0"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("timeout_secs"));
        assert!(err_msg.contains("at least 1"));
    }

    #[test]
    fn load_rejects_timeout_secs_too_large() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = 86401"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("timeout_secs"));
        assert!(err_msg.contains("at most 86400"));
    }

    #[test]
    fn load_accepts_timeout_secs_at_lower_bound() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = 1"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_timeout_secs(), Some(1));
    }

    #[test]
    fn load_accepts_timeout_secs_at_upper_bound() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = 86400"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_timeout_secs(), Some(86400));
    }

    #[test]
    fn load_rejects_empty_model_name() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = ''"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("model"));
        assert!(err_msg.contains("cannot be empty"));
    }

    #[test]
    fn load_rejects_model_name_with_invalid_chars() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "model@bad""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("model"));
        assert!(err_msg.contains("invalid characters"));
    }

    #[test]
    fn load_rejects_model_name_too_long() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        let long_model = "a".repeat(101);
        std::fs::write(
            &config_path,
            format!(
                r#"[defaults]
model = "{}""#,
                long_model
            ),
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("model"));
        assert!(err_msg.contains("too long"));
    }

    #[test]
    fn load_accepts_model_name_with_hyphens() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4-8""#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
    }

    #[test]
    fn load_accepts_model_name_with_underscores() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus_4_8_testing""#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_model(), Some("claude-opus_4_8_testing"));
    }

    #[test]
    fn load_accepts_model_name_with_dots() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus.4.8.testing""#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_model(), Some("claude-opus.4.8.testing"));
    }

    #[test]
    fn load_rejects_model_name_not_starting_with_claude() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "gpt-4""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("model"));
        assert!(err_msg.contains("must start with 'claude-'"));
    }

    #[test]
    fn load_rejects_model_name_starting_with_uppercase_claude() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "Claude-opus-4-8""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("model"));
        assert!(err_msg.contains("must start with 'claude-'"));
    }

    #[test]
    fn load_accepts_model_name_starting_with_claude_hyphen() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4-8""#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_model(), Some("claude-opus-4-8"));
    }

    #[test]
    fn load_accepts_model_name_claude_haiku() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-haiku-4-5-20251001""#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.default_model(), Some("claude-haiku-4-5-20251001"));
    }

    #[test]
    fn load_or_default_warns_on_invalid_max_turns() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 0"#,
        )
        .unwrap();

        let result = Config::load_or_default(&config_path);
        // Should return error on validation error
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("max_turns"));
    }

    #[test]
    fn load_or_default_warns_on_invalid_timeout_secs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = 999999"#,
        )
        .unwrap();

        let result = Config::load_or_default(&config_path);
        // Should return error on validation error
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("timeout_secs"));
    }

    #[test]
    fn load_or_default_warns_on_invalid_model_name() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "bad@model""#,
        )
        .unwrap();

        let result = Config::load_or_default(&config_path);
        // Should return error on validation error
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("model"));
    }

    #[test]
    fn load_rejects_multiple_invalid_fields() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = ""
max_turns = 0
timeout_secs = 0"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        // Should report the first validation error encountered
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("config validation failed"));
    }

    #[test]
    fn load_error_message_includes_field_name() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 50
timeout_secs = 100000"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Error message should clearly state which field is invalid
        assert!(err_msg.contains("timeout_secs"));
        assert!(err_msg.contains("at most 86400"));
    }

    #[test]
    fn load_error_message_includes_reason() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 0"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // Error message should explain why the value is invalid
        assert!(err_msg.contains("must be at least 1"));
    }

    #[test]
    fn default_path_fails_when_home_not_set() {
        let _lock = env_lock();
        let _home = EnvGuard::remove("HOME");
        let _xdg = EnvGuard::remove("XDG_CONFIG_HOME");

        let result = Config::default_path();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("HOME environment variable not set"));
    }

    // Config parse error tests - verify malformed configs produce proper errors
    // These tests should FAIL initially (expecting current buggy behavior),
    // then pass after proper error propagation is implemented.

    #[test]
    fn load_rejects_invalid_toml_syntax_mismatched_brackets() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults
model = "claude-opus-4-8""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject TOML with mismatched brackets"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_invalid_toml_syntax_unclosed_string() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4-8"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err(), "Should reject TOML with unclosed string");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_invalid_toml_syntax_invalid_escape() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4\-8""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject TOML with invalid escape sequence"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_invalid_toml_syntax_double_key() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "claude-opus-4-8"
model = "claude-haiku-4-5""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err(), "Should reject TOML with duplicate keys");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_model_as_integer() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = 123"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject model field as integer (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_model_as_boolean() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = true"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject model field as boolean (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_inherit_hooks_as_string() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
inherit_hooks = "true""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject inherit_hooks field as string (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_max_turns_as_string() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = "50""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject max_turns field as string (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_max_turns_as_float() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = 50.5"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject max_turns field as float (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_timeout_secs_as_string() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = "3600""#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject timeout_secs field as string (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_wrong_type_timeout_secs_as_boolean() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = true"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject timeout_secs field as boolean (wrong type)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_negative_max_turns() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = -10"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject negative max_turns (u32 can't be negative)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_rejects_negative_timeout_secs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
timeout_secs = -100"#,
        )
        .unwrap();

        let result = Config::load(&config_path);
        assert!(
            result.is_err(),
            "Should reject negative timeout_secs (u64 can't be negative)"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_or_default_rejects_invalid_toml_syntax() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(&config_path, r#"[defaults"#).unwrap();

        let result = Config::load_or_default(&config_path);
        assert!(
            result.is_err(),
            "Should not silently fallback to defaults on invalid TOML"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_or_default_rejects_wrong_type_model() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = 456"#,
        )
        .unwrap();

        let result = Config::load_or_default(&config_path);
        assert!(
            result.is_err(),
            "Should not silently fallback to defaults on wrong type"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_or_default_rejects_wrong_type_max_turns() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
max_turns = "not-a-number""#,
        )
        .unwrap();

        let result = Config::load_or_default(&config_path);
        assert!(
            result.is_err(),
            "Should not silently fallback to defaults on wrong type"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("invalid config"),
            "Error should mention invalid config"
        );
    }

    #[test]
    fn load_or_default_rejects_validation_error_in_model() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(
            &config_path,
            r#"[defaults]
model = "gpt-4""#,
        )
        .unwrap();

        let result = Config::load_or_default(&config_path);
        assert!(
            result.is_err(),
            "Should not silently fallback on validation error"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("config validation failed"),
            "Error should mention validation failure"
        );
    }

    #[test]
    fn load_or_default_accepts_minimal_valid_config() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("test-config.toml");
        std::fs::write(&config_path, r#"[defaults]"#).unwrap();

        let config = Config::load_or_default(&config_path).unwrap();
        assert!(
            config.defaults.is_some(),
            "Should accept minimal valid defaults section"
        );
        assert!(
            config.defaults.unwrap().model.is_none(),
            "Model should be None when not specified"
        );
    }
}
