use std::path::PathBuf;

/// `%APPDATA%\BlinkerUI\blinker_ui_config.json`
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("BlinkerUI").join("blinker_ui_config.json")
}

/// Legacy location: next to the executable.
pub fn legacy_config_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("blinker_ui_config.json")))
}
