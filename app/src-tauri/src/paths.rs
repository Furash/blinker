//! Config location.
//!
//! Developer environment variables (see also docs/dev-notes.md):
//! - `BLINKER_CONFIG_DIR`: use `<dir>\blinker_ui_config.json` (and `<dir>\icons`) instead
//!   of `%APPDATA%\BlinkerUI`; the exe-dir legacy config is then not migrated.
//! - `BLINKER_READY_LOG`: file the UI's `report_ready` appends `ready_ms=<ms> timeline=<json>`
//!   to when the window is shown (commands.rs; used by tools/measure_startup.ps1).

use std::ffi::OsString;
use std::path::PathBuf;

/// Environment variable that replaces the config directory (testing, side-by-side runs).
/// When set, the config is `<dir>\blinker_ui_config.json` and the legacy exe-dir config
/// is not migrated, so a run never touches the real `%APPDATA%\BlinkerUI`.
pub const CONFIG_DIR_ENV: &str = "BLINKER_CONFIG_DIR";

/// `%APPDATA%\BlinkerUI\blinker_ui_config.json`, or `%BLINKER_CONFIG_DIR%\blinker_ui_config.json`.
pub fn config_path() -> PathBuf {
    config_path_from(std::env::var_os(CONFIG_DIR_ENV), std::env::var_os("APPDATA"))
}

/// True when the config directory is overridden by `BLINKER_CONFIG_DIR`.
pub fn config_dir_overridden() -> bool {
    override_dir(std::env::var_os(CONFIG_DIR_ENV)).is_some()
}

fn override_dir(v: Option<OsString>) -> Option<PathBuf> {
    v.filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn config_path_from(override_: Option<OsString>, appdata: Option<OsString>) -> PathBuf {
    let dir = override_dir(override_).unwrap_or_else(|| {
        appdata.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("BlinkerUI")
    });
    dir.join("blinker_ui_config.json")
}

/// Legacy location: next to the executable.
pub fn legacy_config_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("blinker_ui_config.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_appdata_blinkerui() {
        let p = config_path_from(None, Some("C:\\AD".into()));
        assert_eq!(p, PathBuf::from("C:\\AD").join("BlinkerUI").join("blinker_ui_config.json"));
    }

    #[test]
    fn override_dir_replaces_appdata_folder() {
        let p = config_path_from(Some("D:\\tmp\\cfg".into()), Some("C:\\AD".into()));
        assert_eq!(p, PathBuf::from("D:\\tmp\\cfg").join("blinker_ui_config.json"));
    }

    #[test]
    fn empty_override_is_ignored() {
        let p = config_path_from(Some("".into()), Some("C:\\AD".into()));
        assert_eq!(p, PathBuf::from("C:\\AD").join("BlinkerUI").join("blinker_ui_config.json"));
        assert_eq!(override_dir(Some("".into())), None);
    }

    #[test]
    fn no_appdata_falls_back_to_cwd() {
        let p = config_path_from(None, None);
        assert_eq!(p, PathBuf::from(".").join("BlinkerUI").join("blinker_ui_config.json"));
    }
}
