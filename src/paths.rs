use std::path::PathBuf;

fn get_home() -> Result<String, Box<dyn std::error::Error>> {
    std::env::var("HOME")
        .map_err(|_| "HOME environment variable not set".into())
}

/// Get the XDG state home directory.
/// Uses $XDG_STATE_HOME if set, otherwise falls back to $HOME/.local/state.
fn get_xdg_state_home() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        return Ok(PathBuf::from(state_home));
    }
    Ok(PathBuf::from(get_home()?).join(".local").join("state"))
}

/// Get the XDG config home directory.
/// Uses $XDG_CONFIG_HOME if set, otherwise falls back to $HOME/.config.
fn get_xdg_config_home() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(config_home));
    }
    Ok(PathBuf::from(get_home()?).join(".config"))
}

/// Get the config file path: $XDG_CONFIG_HOME/mux/config.toml
pub fn get_config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let config_dir = get_xdg_config_home()?.join("mux");
    std::fs::create_dir_all(&config_dir)?;
    Ok(config_dir.join("config.toml"))
}

/// Get the mux state directory: $XDG_STATE_HOME/mux
/// Creates the directory if it doesn't exist.
pub fn get_state_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mux_dir = get_xdg_state_home()?.join("mux");
    std::fs::create_dir_all(&mux_dir)?;
    Ok(mux_dir)
}

/// Get the database path: $XDG_STATE_HOME/mux/history.db
pub fn get_db_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(get_state_dir()?.join("history.db"))
}

/// Get the log directory path: $XDG_STATE_HOME/mux/logs/
pub fn get_log_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let log_dir = get_state_dir()?.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    Ok(log_dir)
}
