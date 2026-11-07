//! `get_state` and its pure core `build_state`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};
use tauri::State;

use super::state::{lock, AppState};
use crate::config::{self, Config};
use crate::theme;

#[derive(Debug, Clone, Serialize)]
pub struct AppStateDto {
    pub config: Config,
    pub config_path: String,
    pub version: String,
    pub running: BTreeMap<String, bool>,
    pub default_theme: Map<String, Value>,
    pub presets: Vec<String>,
    /// The preset themes, in `presets` order (the Settings preset combo).
    pub preset_themes: Vec<Map<String, Value>>,
    pub color_keys: Vec<[String; 2]>,
    /// DEFAULT_TERMINAL / DEFAULT_AI_ALIASES (Settings: Restore Defaults, hint text).
    pub default_terminal: String,
    pub default_ai_aliases: String,
}

/// Pure core of `get_state` (MainWindow.__init__ 1042-1065): favourites first,
/// theme merged over the default preset. Defaults for terminal/ai_aliases are
/// already applied by `config::load`.
pub fn build_state(cfg: &Config, config_path: &Path, running: &BTreeMap<u16, bool>) -> AppStateDto {
    let mut config = cfg.clone();
    config.folders = config::sorted_folders(config.folders);
    config.theme = theme::merge_with_default(&config.theme);
    AppStateDto {
        config,
        config_path: config_path.to_string_lossy().into_owned(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        running: running.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        default_theme: theme::preset(theme::PRESET_NAMES[0]).unwrap_or_default(),
        presets: theme::PRESET_NAMES.iter().map(|s| s.to_string()).collect(),
        preset_themes: theme::PRESET_NAMES.iter().filter_map(|n| theme::preset(n)).collect(),
        color_keys: theme::COLOR_KEYS.iter().map(|(k, l)| [k.to_string(), l.to_string()]).collect(),
        default_terminal: config::default_terminal(),
        default_ai_aliases: config::DEFAULT_AI_ALIASES.to_string(),
    }
}

/// Returns at once: `running` is empty. Probing here blocked startup for ~1 s (a
/// connect to a closed localhost port waits for the full probe timeout on Windows);
/// the UI calls `probe_now` once its listeners are in place, as Python polls right
/// after the window is built (`QTimer.singleShot(0, self._poll_status)`).
#[tauri::command]
pub async fn get_state(state: State<'_, AppState>) -> Result<AppStateDto, String> {
    let cfg = lock(&state.config).clone();
    Ok(build_state(&cfg, &state.config_path, &BTreeMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Folder;
    use std::fs;

    /// Content of the repo's real sample `blinker_ui_config.json`.
    const LEGACY_SAMPLE: &str = r#"{
  "folders": [
    { "path": "D:/git/Blender_CCP_Tools", "port": 9876, "repo": "blinker", "module": "", "blend": "" }
  ],
  "launchers": [
    { "name": "CLAUDE", "command": "wt -d {path} -NoExit -Command claude" }
  ]
}"#;

    #[test]
    fn get_state_with_legacy_config_returns_folder_and_defaults() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("blinker_ui_config.json");
        fs::write(&p, LEGACY_SAMPLE).unwrap();
        let cfg = config::load(&p);
        let s = build_state(&cfg, &p, &BTreeMap::from([(9876, true)]));
        assert_eq!(s.config.folders.len(), 1);
        assert_eq!(s.config.folders[0].path, "D:/git/Blender_CCP_Tools");
        assert_eq!(s.config.folders[0].port, 9876);
        assert_eq!(s.config.ai_aliases, "claude, codex");
        assert_eq!(s.config.terminal, config::default_terminal());
        assert_eq!(s.config.blender_path, "");
        assert_eq!(s.config.theme, theme::merge_with_default(&Map::new()));
        assert_eq!(s.default_theme, theme::preset("Tokyo Night (default)").unwrap());
        assert_eq!(s.presets, vec!["Tokyo Night (default)", "Frigus Nox", "Light"]);
        assert_eq!(s.color_keys.len(), 18);
        assert_eq!(s.preset_themes.len(), 3);
        assert_eq!(s.preset_themes[1], theme::preset("Frigus Nox").unwrap());
        assert_eq!(s.default_terminal, config::default_terminal());
        assert_eq!(s.default_ai_aliases, "claude, codex");
        assert_eq!(s.running.get("9876"), Some(&true));
        assert_eq!(s.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(s.config_path, p.to_string_lossy());
        // unknown keys survive the round trip to the UI
        let j = serde_json::to_value(&s).unwrap();
        assert!(j["config"]["launchers"].is_array());
    }

    #[test]
    fn build_state_sorts_favourites_first_and_merges_user_theme() {
        let mut cfg = Config::default();
        cfg.folders = vec![
            Folder { path: "a".into(), ..Folder::default() },
            Folder { path: "b".into(), favourite: true, ..Folder::default() },
        ];
        let key = theme::COLOR_KEYS[0].0;
        cfg.theme.insert(key.into(), Value::String("#000000".into()));
        let s = build_state(&cfg, Path::new("c.json"), &BTreeMap::new());
        assert_eq!(s.config.folders[0].path, "b");
        assert_eq!(s.config.theme.get(key), Some(&Value::String("#000000".into())));
        assert_eq!(s.config.theme.len(), s.default_theme.len());
    }
}
