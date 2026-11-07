//! Saving the shared config and scanning for addons.

use std::path::Path;

use serde::Serialize;
use tauri::State;

use super::state::{lock, AppState};
use crate::addons::{self, AddonKind};
use crate::config::{self, Config};

/// Entries the UI never sees (unparsable folders, wrongly typed values) are
/// `serde(skip)` fields: carry them over from the managed config.
pub fn merge_for_save(incoming: Config, current: &Config) -> Config {
    let mut cfg = incoming;
    cfg.unparsed_folders = current.unparsed_folders.clone();
    cfg.invalid = current.invalid.clone();
    cfg
}

/// The single write path for the shared config. The caller holds the config mutex
/// (`slot` is the guarded value), so no two writers can interleave on the `.tmp` file.
pub(super) fn commit_config(path: &Path, slot: &mut Config, new: Config) -> std::io::Result<()> {
    let r = config::save(path, &new);
    if r.is_ok() {
        *slot = new;
    }
    r
}

#[tauri::command]
pub async fn save_config(state: State<'_, AppState>, config: Config) -> Result<(), String> {
    let mut slot = lock(&state.config);
    let cfg = merge_for_save(config, &slot);
    commit_config(&state.config_path, &mut slot, cfg).map_err(|e| format!("Could not save config: {e}"))
}

#[derive(Debug, Serialize)]
pub struct AddonEntry {
    pub path: String,
    pub kind: AddonKind,
}

#[tauri::command]
pub async fn scan_addons(root: String) -> Result<Vec<AddonEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        addons::find_addons(Path::new(&root), 3)
            .into_iter()
            .map(|(p, kind)| AddonEntry { path: p.to_string_lossy().into_owned(), kind })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn addon_kind_of(path: String) -> Result<Option<AddonKind>, String> {
    Ok(addons::addon_kind(Path::new(&path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use super::super::get_state::build_state;
    use crate::config::Folder;
    use std::fs;

    #[test]
    fn merge_for_save_preserves_unparsed_invalid_and_unknown_through_ui_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("in.json");
        fs::write(
            &src,
            r#"{"folders":[{"path":"a","port":9876},{"path":"bad","port":"x"}],
                "terminal":5,"mystery":{"k":1}}"#,
        )
        .unwrap();
        let loaded = config::load(&src);
        assert_eq!(loaded.unparsed_folders.len(), 1);
        // as get_state hands it to the UI and the UI returns it
        let dto = build_state(&loaded, &src, &BTreeMap::new());
        let json = serde_json::to_string(&dto.config).unwrap();
        let incoming: Config = serde_json::from_str(&json).unwrap();
        assert!(incoming.unparsed_folders.is_empty() && incoming.invalid.is_empty());
        let merged = merge_for_save(incoming, &loaded);
        let out = t.path().join("out.json");
        config::save(&out, &merged).unwrap();
        let back = config::load(&out);
        assert_eq!(back.unparsed_folders.len(), 1);
        assert_eq!(back.unparsed_folders[0]["path"], "bad");
        assert_eq!(back.invalid.get("terminal"), Some(&serde_json::json!(5)));
        assert_eq!(back.extra.get("mystery"), Some(&serde_json::json!({"k":1})));
        assert_eq!(back.folders.len(), 1);
    }

    #[test]
    fn merge_for_save_edited_folders_replace_but_unparsed_kept_once() {
        let mut current = Config::default();
        current.folders = vec![Folder { path: "old".into(), ..Folder::default() }];
        current.unparsed_folders = vec![serde_json::json!({"path": 1})];
        let mut incoming = Config::default();
        incoming.folders = vec![
            Folder { path: "n1".into(), ..Folder::default() },
            Folder { path: "n2".into(), ..Folder::default() },
        ];
        let m = merge_for_save(incoming, &current);
        assert_eq!(m.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["n1", "n2"]);
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("c.json");
        config::save(&p, &m).unwrap();
        let back = config::load(&p);
        assert_eq!(back.folders.len(), 2);
        assert_eq!(back.unparsed_folders.len(), 1);
    }

    #[test]
    fn commit_config_replaces_slot_only_on_success() {
        let t = tempfile::tempdir().unwrap();
        let mut slot = Config::default();
        let mut new = Config::default();
        new.blender_path = "b".into();
        commit_config(&t.path().join("c.json"), &mut slot, new.clone()).unwrap();
        assert_eq!(slot.blender_path, "b");
        // a directory in place of the file makes the rename fail
        let bad = t.path().join("dir");
        fs::create_dir(&bad).unwrap();
        let mut other = Config::default();
        other.blender_path = "z".into();
        assert!(commit_config(&bad, &mut slot, other).is_err());
        assert_eq!(slot.blender_path, "b");
    }
}
