//! Import Config (import.rs): inspect a file, then replace the setup with it.

use std::collections::BTreeMap;
use std::path::Path;

use tauri::State;

use super::config_cmds::commit_config;
use super::get_state::build_state;
use super::state::{lock, AppState};
use crate::config::{self, Config};
use crate::import;

// ----- Import Config (import.rs) -----

/// What the confirm dialog shows about a candidate `blinker_ui_config.json`.
#[tauri::command]
pub async fn inspect_config_file(path: String) -> Result<import::ConfigFileSummary, String> {
    import::read_config_file(Path::new(&path)).map(|p| p.summary)
}

const FILE_CHANGED: &str = "The file changed since you chose it; select it again.";

/// Import step 1, no locks: read the file again (the earlier inspect is not trusted),
/// refuse when its content is not the one the user confirmed (`fingerprint` from
/// `inspect_config_file`), and build the config to store: the state the UI would get at
/// startup for this file (`build_state`: favourites first, theme merged over the default,
/// defaults applied). The imported file's own skipped data (unknown keys, wrongly typed
/// values, unparsable folders) stays in it, so the next `save_config` (which carries the
/// slot's skipped data over, `merge_for_save`) keeps the imported ones and drops the old.
/// Touches nothing on disk.
pub fn prepare_import(config_path: &Path, src: &Path, fingerprint: &str) -> Result<Config, String> {
    let parsed = import::read_config_file(src)?;
    if parsed.summary.fingerprint != fingerprint {
        return Err(FILE_CHANGED.into());
    }
    Ok(build_state(&parsed.config, config_path, &BTreeMap::new()).config)
}

/// Import step 2, the only part under the config mutex (`slot` is the guarded value):
/// refuse while a supervised Blender is alive (`alive` must be read inside the same
/// critical section, right before this call), copy the previous on-disk config to
/// `<config>.bak`, then write through `commit_config`, the single writer. A launch that
/// starts after the alive check but before the commit is tolerated (the UI refuses to
/// import while one is pending).
pub fn commit_import(config_path: &Path, slot: &mut Config, new: Config, alive: &[u16]) -> Result<Config, String> {
    if !alive.is_empty() {
        return Err("Stop the running Blender instances first, then import again.".into());
    }
    config::backup(config_path).map_err(|e| format!("Could not back up the current config: {e}"))?;
    commit_config(config_path, slot, new.clone()).map_err(|e| format!("Could not save config: {e}"))?;
    Ok(new)
}

/// Both steps for callers that already hold the lock (tests, the debug self-test).
#[cfg(any(test, debug_assertions))]
pub fn apply_import(
    config_path: &Path,
    slot: &mut Config,
    src: &Path,
    fingerprint: &str,
    alive: &[u16],
) -> Result<Config, String> {
    let new = prepare_import(config_path, src, fingerprint)?;
    commit_import(config_path, slot, new, alive)
}

/// Import Config, step 2 of the UI flow: replace the setup with the file's, provided it
/// still has the content `inspect_config_file` fingerprinted. The returned config has the
/// shape of `get_state().config`; the UI swaps it in. Reading and parsing happen before
/// the config mutex is taken; lock order is config -> supervisor.
#[tauri::command]
pub async fn apply_config_file(state: State<'_, AppState>, path: String, fingerprint: String) -> Result<Config, String> {
    let new = prepare_import(&state.config_path, Path::new(&path), &fingerprint)?;
    let mut slot = lock(&state.config);
    let alive = state.supervisor.alive_ports();
    commit_import(&state.config_path, &mut slot, new, &alive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config_cmds::merge_for_save;
    use crate::blender::{LaunchSpec, OutputSink, Supervisor};
    use crate::config::Folder;
    use crate::theme;
    use serde_json::{Map, Value};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use std::fs;

    fn write(p: &Path, s: &str) -> PathBuf {
        fs::write(p, s).unwrap();
        p.to_path_buf()
    }

    /// What the UI does with a config from `apply_config_file` / `get_state`: it comes
    /// back through JSON (skipped fields are `serde(skip)`) and `save_config` merges it.
    fn ui_save(path: &Path, slot: &mut Config, ui: &Config) {
        let incoming: Config = serde_json::from_str(&serde_json::to_string(ui).unwrap()).unwrap();
        let merged = merge_for_save(incoming, slot);
        commit_config(path, slot, merged).unwrap();
    }

    fn json_of(p: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    /// `apply_import` with the fingerprint `inspect_config_file` would give right now.
    fn apply(s: &Setup, slot: &mut Config, alive: &[u16]) -> Result<Config, String> {
        let fp = import::read_config_file(&s.src).map(|p| p.summary.fingerprint).unwrap_or_default();
        apply_import(&s.cfg, slot, &s.src, &fp, alive)
    }

    struct Setup {
        _t: tempfile::TempDir,
        cfg: PathBuf,
        src: PathBuf,
    }

    fn setup(current: Option<&str>, imported: &str) -> (Setup, Config) {
        let t = tempfile::tempdir().unwrap();
        let cfg = t.path().join("blinker_ui_config.json");
        let src = write(&t.path().join("other.json"), imported);
        if let Some(c) = current {
            write(&cfg, c);
        }
        let slot = config::load(&cfg);
        (Setup { _t: t, cfg, src }, slot)
    }

    #[test]
    fn apply_import_replaces_everything_and_returns_the_get_state_shape() {
        let (s, mut slot) = setup(
            Some(r#"{"folders":[{"path":"old","port":9000}],"terminal":"old term","blender_path":"B","launchers":[1]}"#),
            crate::import::tests::OLD_PYTHON_CONFIG,
        );
        let got = apply(&s, &mut slot, &[]).unwrap();
        // same as get_state would hand the UI for the imported file
        let expect = build_state(&config::load(&s.src), &s.cfg, &BTreeMap::new()).config;
        assert_eq!(got, expect);
        assert_eq!(got.folders.len(), 7);
        assert_eq!(got.folders[0].path, r"D:\git\Blender_CCP_Tools");
        assert_eq!(got.theme, theme::merge_with_default(&config::load(&s.src).theme));
        assert_eq!(got.blender_path, "", "the old blender_path is gone");
        assert!(got.extra.is_empty(), "the old unknown keys are gone");
        assert_eq!(slot, got);
        // disk == imported content (modulo defaults: theme merged over the preset)
        let disk = config::load(&s.cfg);
        assert_eq!(disk, got);
        let ports: Vec<u16> = disk.folders.iter().map(|f| f.port).collect();
        assert_eq!(ports, [9901, 9900, 9876, 9910, 9907, 9910, 9877], "duplicate ports kept");
        assert!(!s.cfg.with_file_name("blinker_ui_config.json.tmp").exists());
    }

    #[test]
    fn apply_import_sorts_favourites_first_and_defaults_missing_fields() {
        let (s, mut slot) = setup(None, r#"{"folders":[{"path":"a"},{"path":"b","favourite":true}]}"#);
        let got = apply(&s, &mut slot, &[]).unwrap();
        assert_eq!(got.folders.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(got.terminal, config::default_terminal());
        assert_eq!(got.ai_aliases, "claude, codex");
        assert_eq!(got.theme, theme::merge_with_default(&Map::new()));
        assert!(s.cfg.is_file(), "created although there was no config");
    }

    #[test]
    fn apply_import_keeps_the_imported_skipped_data_through_the_next_ui_save() {
        let (s, mut slot) = setup(
            Some(r#"{"folders":[{"path":"old"},{"path":"oldbad","port":"x"}],"terminal":5,"oldkey":1}"#),
            r#"{"folders":[{"path":"n1"},{"path":"badport","port":99999},"junk"],"ai_aliases":9,"newkey":{"k":2}}"#,
        );
        assert_eq!(slot.unparsed_folders.len(), 1);
        let got = apply(&s, &mut slot, &[]).unwrap();
        // the UI edits something and saves
        let mut ui = got.clone();
        ui.blender_path = "C:/blender.exe".into();
        ui_save(&s.cfg, &mut slot, &ui);
        let v = json_of(&s.cfg);
        let folders = v["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 3, "n1 + the two unparsable imported entries");
        assert!(folders.iter().any(|f| f["port"] == 99999));
        assert!(folders.iter().any(|f| f == "junk"));
        assert!(!folders.iter().any(|f| f["path"] == "oldbad"), "old unparsable entry not resurrected");
        assert_eq!(v["ai_aliases"], 9, "imported wrongly typed value kept");
        assert_eq!(v["newkey"], serde_json::json!({"k": 2}));
        assert!(v.get("oldkey").is_none(), "old unknown key dropped");
        assert_ne!(v["terminal"], 5, "old wrongly typed value not resurrected");
        assert_eq!(v["blender_path"], "C:/blender.exe");
    }

    #[test]
    fn apply_import_previous_skipped_fields_do_not_leak_when_the_file_has_none() {
        let (s, mut slot) = setup(
            Some(r#"{"folders":[{"path":"oldbad","port":"x"}],"folders_extra":1,"theme":[],"oldkey":true}"#),
            r#"{"folders":[{"path":"n"}]}"#,
        );
        let got = apply(&s, &mut slot, &[]).unwrap();
        assert!(slot.unparsed_folders.is_empty() && slot.invalid.is_empty());
        ui_save(&s.cfg, &mut slot, &got);
        let v = json_of(&s.cfg);
        assert_eq!(v["folders"].as_array().unwrap().len(), 1);
        assert!(v.get("oldkey").is_none() && v.get("folders_extra").is_none());
        assert!(v["theme"].is_object(), "the old wrongly typed theme is not written back");
    }

    #[test]
    fn apply_import_writes_and_replaces_a_single_bak_of_the_previous_config() {
        let first = r#"{"folders":[{"path":"first"}]}"#;
        let (s, mut slot) = setup(Some(first), r#"{"folders":[{"path":"second"}]}"#);
        let bak = config::backup_path(&s.cfg);
        assert!(bak.to_string_lossy().ends_with("blinker_ui_config.json.bak"));
        apply(&s, &mut slot, &[]).unwrap();
        assert_eq!(fs::read_to_string(&bak).unwrap(), first, "byte-exact copy of the previous file");
        let second_on_disk = fs::read_to_string(&s.cfg).unwrap();
        write(&s.src, r#"{"folders":[{"path":"third"}]}"#);
        apply(&s, &mut slot, &[]).unwrap();
        assert_eq!(fs::read_to_string(&bak).unwrap(), second_on_disk, "overwritten, not accumulated");
        let names: Vec<_> = fs::read_dir(s.cfg.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 3, "config, .bak and the source file only: {names:?}");
    }

    #[test]
    fn apply_import_without_a_previous_file_writes_no_bak() {
        let (s, mut slot) = setup(None, r#"{"folders":[]}"#);
        apply(&s, &mut slot, &[]).unwrap();
        assert!(!config::backup_path(&s.cfg).exists());
    }

    #[test]
    fn apply_import_refuses_while_a_supervised_blender_is_alive() {
        struct Quiet;
        impl OutputSink for Quiet {
            fn push(&self, _: u16, _: &str, _: u64) {}
            fn exited(&self, _: u16, _: Option<i32>) {}
        }
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().join("temp");
        fs::create_dir_all(&tmp).unwrap();
        let addon = t.path().join("addon");
        fs::create_dir_all(&addon).unwrap();
        fs::write(addon.join("__init__.py"), b"").unwrap();
        let boot = t.path().join("bootstrap.py");
        fs::write(&boot, b"#").unwrap();
        let sup = Supervisor::with_temp_dir(Arc::new(Quiet), tmp);
        sup.launch(LaunchSpec {
            blender: "cmd.exe".into(),
            bootstrap: boot,
            folder: Folder { path: addon.to_string_lossy().into_owned(), port: 9931, ..Folder::default() },
            blend: None,
            args_override: Some(vec!["/d".into(), "/c".into(), "for /L %i in (1,1,2000000000) do @rem".into()]),
        })
        .unwrap();
        assert_eq!(sup.alive_ports(), vec![9931]);

        let (s, mut slot) = setup(Some(r#"{"folders":[{"path":"keep"}]}"#), r#"{"folders":[{"path":"new"}]}"#);
        let before = fs::read_to_string(&s.cfg).unwrap();
        let before_slot = slot.clone();
        let e = apply(&s, &mut slot, &sup.alive_ports()).unwrap_err();
        assert_eq!(e, "Stop the running Blender instances first, then import again.");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), before);
        assert_eq!(slot, before_slot);
        assert!(!config::backup_path(&s.cfg).exists());

        assert!(sup.kill(9931));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !sup.alive_ports().is_empty() {
            assert!(std::time::Instant::now() < deadline, "stand-in did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        apply(&s, &mut slot, &sup.alive_ports()).unwrap();
        assert_eq!(slot.folders[0].path, "new");
    }

    #[test]
    fn apply_import_rejects_bad_files_before_touching_anything() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        for (bad, msg) in [
            ("{}", "This file does not look like a Blinker UI config (no folders or settings found)"),
            ("[1]", "Config file must be a JSON object"),
            ("", "Expecting value: line 1 column 1 (char 0)"),
        ] {
            let (s, mut slot) = setup(Some(cur), bad);
            let e = apply(&s, &mut slot, &[]).unwrap_err();
            assert_eq!(e, msg);
            assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur);
            assert_eq!(slot.folders[0].path, "keep");
            assert!(!config::backup_path(&s.cfg).exists());
        }
        let (s, mut slot) = setup(Some(cur), "{}");
        let e = apply_import(&s.cfg, &mut slot, &s.cfg.with_file_name("missing.json"), "x", &[]).unwrap_err();
        assert!(e.starts_with("[Errno 2] No such file or directory"), "{e}");
    }

    #[test]
    fn apply_import_commit_failure_leaves_the_old_config_in_place() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        let (s, mut slot) = setup(Some(cur), r#"{"folders":[{"path":"new"}]}"#);
        // a directory where the temp file must go makes the write fail
        fs::create_dir(s.cfg.with_file_name("blinker_ui_config.json.tmp")).unwrap();
        let e = apply(&s, &mut slot, &[]).unwrap_err();
        assert!(e.starts_with("Could not save config: "), "{e}");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur, "config file untouched");
        assert_eq!(slot.folders[0].path, "keep", "managed config untouched");
    }

    #[test]
    fn apply_import_backup_failure_aborts_the_import() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        let (s, mut slot) = setup(Some(cur), r#"{"folders":[{"path":"new"}]}"#);
        fs::create_dir(config::backup_path(&s.cfg)).unwrap(); // a directory in the way
        let e = apply(&s, &mut slot, &[]).unwrap_err();
        assert!(e.starts_with("Could not back up the current config: "), "{e}");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur);
        assert_eq!(slot.folders[0].path, "keep");
    }

    #[test]
    fn apply_import_refuses_a_file_that_changed_since_inspect() {
        let cur = r#"{"folders":[{"path":"keep"}]}"#;
        let (s, mut slot) = setup(Some(cur), r#"{"folders":[{"path":"confirmed"}]}"#);
        let fp = import::read_config_file(&s.src).unwrap().summary.fingerprint;
        // someone swaps the file after the user confirmed it
        write(&s.src, r#"{"folders":[{"path":"swapped"}]}"#);
        let e = apply_import(&s.cfg, &mut slot, &s.src, &fp, &[]).unwrap_err();
        assert_eq!(e, "The file changed since you chose it; select it again.");
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), cur);
        assert_eq!(slot.folders[0].path, "keep");
        assert!(!config::backup_path(&s.cfg).exists(), "no .bak for a refused import");
        // an empty or wrong fingerprint is a mismatch too; the right one imports
        for bad in ["", "abc"] {
            assert_eq!(apply_import(&s.cfg, &mut slot, &s.src, bad, &[]).unwrap_err(), e);
        }
        let fp2 = import::read_config_file(&s.src).unwrap().summary.fingerprint;
        apply_import(&s.cfg, &mut slot, &s.src, &fp2, &[]).unwrap();
        assert_eq!(slot.folders[0].path, "swapped");
    }

    #[test]
    fn prepare_import_builds_the_new_config_without_touching_disk() {
        let (s, slot) = setup(
            Some(r#"{"folders":[{"path":"keep"}]}"#),
            r#"{"folders":[{"path":"a"},{"path":"b","favourite":true}]}"#,
        );
        let before = fs::read_to_string(&s.cfg).unwrap();
        let fp = import::read_config_file(&s.src).unwrap().summary.fingerprint;
        let new = prepare_import(&s.cfg, &s.src, &fp).unwrap();
        assert_eq!(new.folders[0].path, "b");
        assert_eq!(new.theme, theme::merge_with_default(&Map::new()));
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), before);
        assert!(!config::backup_path(&s.cfg).exists());
        assert_eq!(slot.folders[0].path, "keep");
        // the locked step alone is tiny: refusal, then backup + commit
        let mut slot = slot;
        assert!(commit_import(&s.cfg, &mut slot, new.clone(), &[9876]).is_err());
        assert_eq!(fs::read_to_string(&s.cfg).unwrap(), before);
        commit_import(&s.cfg, &mut slot, new, &[]).unwrap();
        assert_eq!(slot.folders[0].path, "b");
        assert_eq!(fs::read_to_string(config::backup_path(&s.cfg)).unwrap(), before);
    }

    #[test]
    fn inspect_command_returns_the_summary() {
        let (s, _) = setup(None, crate::import::tests::OLD_PYTHON_CONFIG);
        let sum = tauri::async_runtime::block_on(inspect_config_file(s.src.to_string_lossy().into_owned())).unwrap();
        assert_eq!(sum.folders, 7);
        assert!(tauri::async_runtime::block_on(inspect_config_file("Z:/nope.json".into())).is_err());
    }
}
