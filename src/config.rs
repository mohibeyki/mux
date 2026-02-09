use serde::Deserialize;
use std::path::Path;

/// Top-level configuration for mux.
///
/// Loaded from `$XDG_CONFIG_HOME/mux/config.toml`.
/// All fields are optional — missing values use defaults.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub runner: RunnerConfig,
    pub output: OutputConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RunnerConfig {
    /// Maximum number of tasks that can run concurrently.
    /// Tasks beyond this limit are queued.
    pub max_concurrent: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OutputConfig {
    /// Maximum number of output lines kept in memory.
    pub max_lines: usize,
    /// Horizontal padding (spaces) inside output boxes.
    pub box_padding_horizontal: usize,
    /// Vertical padding (empty lines) inside output boxes.
    pub box_padding_vertical: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Maximum log file size in megabytes before rotation.
    pub max_file_size_mb: u64,
    /// Number of archived log files to keep.
    pub max_archives: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runner: RunnerConfig::default(),
            output: OutputConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 64,
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            max_lines: 10_000,
            box_padding_horizontal: 1,
            box_padding_vertical: 0,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            max_file_size_mb: 10,
            max_archives: 5,
        }
    }
}

impl Config {
    /// Load config from a TOML file. Returns defaults if the file doesn't exist.
    /// Logs a warning and returns defaults if the file exists but is malformed.
    pub fn load(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => config,
                Err(e) => {
                    log::warn!("Failed to parse config at {}: {}", path.display(), e);
                    Self::default()
                }
            },
            Err(e) => {
                log::warn!("Failed to read config at {}: {}", path.display(), e);
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let config = Config::default();
        assert_eq!(config.runner.max_concurrent, 64);
        assert_eq!(config.output.max_lines, 10_000);
        assert_eq!(config.output.box_padding_horizontal, 1);
        assert_eq!(config.output.box_padding_vertical, 0);
        assert_eq!(config.logging.max_file_size_mb, 10);
        assert_eq!(config.logging.max_archives, 5);
    }

    #[test]
    fn test_partial_toml() {
        let toml = r#"
[runner]
max_concurrent = 8
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.runner.max_concurrent, 8);
        // Others should be defaults
        assert_eq!(config.output.max_lines, 10_000);
        assert_eq!(config.logging.max_file_size_mb, 10);
    }

    #[test]
    fn test_full_toml() {
        let toml = r#"
[runner]
max_concurrent = 16

[output]
max_lines = 5000
box_padding_horizontal = 2
box_padding_vertical = 1

[logging]
max_file_size_mb = 50
max_archives = 10
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.runner.max_concurrent, 16);
        assert_eq!(config.output.max_lines, 5000);
        assert_eq!(config.output.box_padding_horizontal, 2);
        assert_eq!(config.output.box_padding_vertical, 1);
        assert_eq!(config.logging.max_file_size_mb, 50);
        assert_eq!(config.logging.max_archives, 10);
    }

    #[test]
    fn test_missing_file_returns_defaults() {
        let config = Config::load(Path::new("/nonexistent/path/config.toml"));
        assert_eq!(config.runner.max_concurrent, 64);
    }
}
