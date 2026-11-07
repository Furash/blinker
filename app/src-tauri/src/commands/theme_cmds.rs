//! Settings dialog theme commands (theme.rs holds the one implementation).

use std::path::Path;

use serde_json::{Map, Value};

use crate::theme;

// ----- Settings dialog: one implementation of the theme helpers (theme.rs) -----

/// `SettingsDialog._normalize_hex`.
#[tauri::command]
pub fn normalize_hex(s: String) -> Option<String> {
    theme::normalize_hex(&s)
}

/// `SettingsDialog._detect_preset`: preset name or "Custom".
#[tauri::command]
pub fn detect_preset(theme: Map<String, Value>) -> String {
    crate::theme::detect_preset(&theme)
}

/// `_export_theme` after the save dialog: write the draft theme as JSON. The error
/// text is what the "Export failed" box shows.
#[tauri::command]
pub async fn export_theme(path: String, theme: Map<String, Value>) -> Result<(), String> {
    crate::theme::write_theme_file(Path::new(&path), &theme)
}

/// `_import_theme` after the open dialog: accepted values, unknown and invalid keys.
#[tauri::command]
pub async fn import_theme(path: String) -> Result<theme::ImportedTheme, String> {
    theme::read_theme_file(Path::new(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_commands_wrap_theme_rs() {
        assert_eq!(normalize_hex(" #ABC".into()).as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex("nope".into()), None);
        assert_eq!(detect_preset(theme::preset("Light").unwrap()), "Light");
        let mut t = theme::preset("Light").unwrap();
        t.insert("accent_fg".into(), Value::from("#000000"));
        assert_eq!(detect_preset(t), "Custom");
        // the command's JSON shape (what the dialog reads)
        let r = theme::parse_theme_import(r#"{"x": 1, "ui_font_size": "y"}"#).unwrap();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({"theme": {}, "unknown": ["x"], "invalid": ["ui_font_size"]})
        );
    }

    #[test]
    fn export_and_import_commands_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("t.json").to_string_lossy().into_owned();
        let theme = theme::preset("Frigus Nox").unwrap();
        tauri::async_runtime::block_on(export_theme(p.clone(), theme.clone())).unwrap();
        let back = tauri::async_runtime::block_on(import_theme(p)).unwrap();
        assert_eq!(back.theme, theme);
        let bad = t.path().to_string_lossy().into_owned(); // a directory
        assert!(tauri::async_runtime::block_on(export_theme(bad.clone(), theme)).is_err());
        assert!(tauri::async_runtime::block_on(import_theme(bad)).is_err());
    }
}
