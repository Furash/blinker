//! Import Config: read a `blinker_ui_config.json` from anywhere and describe it
//! (`inspect_config_file`), or hand the parsed config to the single writer
//! (`commands::apply_config_file`). The file goes through the same lenient loader as the
//! managed config, so unknown keys, wrongly typed values and unparsable folder entries
//! are kept exactly as `config::load` keeps them.

use std::io::Read;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::config::{self, Config};

/// Larger files are not configs; refuse before reading them into memory.
pub const MAX_CONFIG_BYTES: u64 = 5 * 1024 * 1024;

const NOT_AN_OBJECT: &str = "Config file must be a JSON object";
const NOT_A_CONFIG: &str = "This file does not look like a Blinker UI config (no folders or settings found)";

/// What the confirm dialog shows about a candidate file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigFileSummary {
    /// SHA-256 (lower-case hex) of the bytes that were read. `apply_config_file` takes it
    /// back and refuses when the file no longer has this content.
    pub fingerprint: String,
    pub folders: usize,
    /// Folder entries that cannot be represented; kept verbatim in the file, not shown.
    pub skipped_folders: usize,
    pub has_theme: bool,
    pub has_terminal: bool,
    pub has_ai_aliases: bool,
    pub has_blender_path: bool,
    /// The first five folder paths, in file order.
    pub sample_paths: Vec<String>,
}

/// A file read through the lenient loader, with its summary.
#[derive(Debug)]
pub struct ParsedConfig {
    pub config: Config,
    pub summary: ConfigFileSummary,
}

pub fn read_config_file(path: &Path) -> Result<ParsedConfig, String> {
    read_config_file_limited(path, MAX_CONFIG_BYTES)
}

pub fn read_config_file_limited(path: &Path, max_bytes: u64) -> Result<ParsedConfig, String> {
    let file = std::fs::File::open(path).map_err(|e| crate::pyerr::os_error(&e, path))?;
    // One byte past the cap tells "too large" without reading a huge file.
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes).map_err(|e| crate::pyerr::os_error(&e, path))?;
    if bytes.len() as u64 > max_bytes {
        let limit = if max_bytes % (1024 * 1024) == 0 {
            format!("{} MB", max_bytes / (1024 * 1024))
        } else {
            format!("{max_bytes} bytes")
        };
        return Err(format!("Config file is too large (limit {limit})"));
    }
    parse_config_bytes(&bytes)
}

/// The file's bytes as the importer sees them: invalid UTF-8 replaced and a BOM dropped
/// (as `config::load` does), then Python's `json.loads` error texts, then the lenient
/// field-by-field loader. Texts Python's decoder accepts but serde_json does not (NaN,
/// Infinity, very deep nesting) fail with serde's message.
pub fn parse_config_bytes(bytes: &[u8]) -> Result<ParsedConfig, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    if let Some(msg) = crate::pyerr::json_error(&text) {
        return Err(msg);
    }
    let Value::Object(obj) = serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())? else {
        return Err(NOT_AN_OBJECT.into());
    };
    // Content that counts: a folder list (even an empty one) or any other known key with
    // its right type. A file with only wrong-typed known keys would import as an empty
    // setup, so it is not taken for a config.
    let typed = |k: &str, ok: fn(&Value) -> bool| obj.get(k).is_some_and(ok);
    let recognised = typed("folders", Value::is_array)
        || typed("terminal", Value::is_string)
        || typed("ai_aliases", Value::is_string)
        || typed("blender_path", Value::is_string)
        || typed("theme", Value::is_object);
    if !recognised {
        return Err(NOT_A_CONFIG.into());
    }
    let non_empty = |k: &str| obj.get(k).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
    let has_theme = obj.get("theme").and_then(Value::as_object).is_some_and(|m| !m.is_empty());
    let has_terminal = non_empty("terminal");
    let has_blender_path = non_empty("blender_path");
    let has_ai_aliases = obj.get("ai_aliases").is_some_and(Value::is_string);
    let config = config::from_object(obj);
    let fingerprint = crate::updater::sha256_hex_reader(bytes)?;
    let summary = ConfigFileSummary {
        fingerprint,
        folders: config.folders.len(),
        skipped_folders: config.unparsed_folders.len(),
        has_theme,
        has_terminal,
        has_ai_aliases,
        has_blender_path,
        sample_paths: config.folders.iter().take(5).map(|f| f.path.clone()).collect(),
    };
    Ok(ParsedConfig { config, summary })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// Modelled on the owner's old Python-app config: 7 folders (two share port 9910),
    /// Windows paths with backslashes (one trailing), the five known keys.
    pub const OLD_PYTHON_CONFIG: &str = r##"{
  "folders": [
    { "path": "D:\\git\\Blender_CCP_Tools", "port": 9901, "repo": "blinker", "module": "", "blend": "", "favourite": true },
    { "path": "D:\\git\\InteractionOps", "port": 9900, "repo": "blinker", "module": "", "blend": "", "favourite": true },
    { "path": "D:\\git\\snap_issue\\", "port": 9876, "repo": "blinker", "module": "", "blend": "", "favourite": true },
    { "path": "D:/git/blender_gr2", "port": 9910, "repo": "blinker", "module": "", "blend": "", "favourite": false },
    { "path": "D:\\git\\chordsong", "port": 9907, "repo": "blinker", "module": "", "blend": "", "favourite": false },
    { "path": "D:\\git\\unwrapme_bl5", "port": 9910, "repo": "blinker", "module": "", "blend": "", "favourite": false },
    { "path": "D:\\git\\cablemess", "port": 9877, "repo": "blinker", "module": "", "blend": "", "favourite": false }
  ],
  "terminal": "start \"\" /D \"{path}\" cmd /K \"{cmd}\"",
  "ai_aliases": "claude, codex",
  "blender_path": "",
  "theme": { "ui_font_family": "Ubuntu Medium", "ui_font_size": 10, "window_bg": "#21252b" }
}"##;

    fn parse(s: &str) -> Result<ParsedConfig, String> {
        parse_config_bytes(s.as_bytes())
    }

    #[test]
    fn old_python_config_is_summarised_and_duplicates_are_kept() {
        let p = parse(OLD_PYTHON_CONFIG).unwrap();
        assert_eq!(p.config.folders.len(), 7);
        let ports: Vec<u16> = p.config.folders.iter().map(|f| f.port).collect();
        assert_eq!(ports, [9901, 9900, 9876, 9910, 9907, 9910, 9877]);
        assert_eq!(p.summary.fingerprint.len(), 64);
        assert!(p.summary.fingerprint.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(
            p.summary,
            ConfigFileSummary {
                fingerprint: p.summary.fingerprint.clone(),
                folders: 7,
                skipped_folders: 0,
                has_theme: true,
                has_terminal: true,
                has_ai_aliases: true,
                has_blender_path: false, // present but empty
                sample_paths: vec![
                    r"D:\git\Blender_CCP_Tools".into(),
                    r"D:\git\InteractionOps".into(),
                    r"D:\git\snap_issue\".into(),
                    "D:/git/blender_gr2".into(),
                    r"D:\git\chordsong".into(),
                ],
            }
        );
        assert_eq!(serde_json::to_value(&p.summary).unwrap()["skipped_folders"], 0);
    }

    #[test]
    fn bom_and_invalid_utf8_are_tolerated_like_the_loader() {
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice(br#"{"folders":[{"path":"a"}]}"#);
        assert_eq!(parse_config_bytes(&b).unwrap().summary.folders, 1);
        let mut c = br#"{"folders":[{"path":"a"#.to_vec();
        c.push(0xFF);
        c.extend_from_slice(br#"b"}]}"#);
        let p = parse_config_bytes(&c).unwrap();
        assert_eq!(p.config.folders[0].path, "a\u{fffd}b");
    }

    #[test]
    fn empty_and_broken_files_report_the_python_json_error() {
        assert_eq!(parse("").unwrap_err(), "Expecting value: line 1 column 1 (char 0)");
        assert_eq!(parse("  \n").unwrap_err(), "Expecting value: line 2 column 1 (char 3)");
        assert_eq!(parse("{\"folders\": [").unwrap_err(), "Expecting value: line 1 column 14 (char 13)");
        assert_eq!(
            parse("{\"a\": 1,}").unwrap_err(),
            "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"
        );
    }

    #[test]
    fn non_object_json_is_rejected() {
        for s in ["[1,2]", "5", "\"x\"", "null", "true"] {
            assert_eq!(parse(s).unwrap_err(), "Config file must be a JSON object", "{s}");
        }
    }

    #[test]
    fn files_without_any_config_content_are_rejected() {
        let want = "This file does not look like a Blinker UI config (no folders or settings found)";
        assert_eq!(parse("{}").unwrap_err(), want);
        // a theme export (colour keys at the top level) and an unrelated JSON file
        assert_eq!(parse(r##"{"window_bg":"#000000","ui_font_size":10}"##).unwrap_err(), want);
        assert_eq!(parse(r#"{"name":"x","version":"1.0"}"#).unwrap_err(), want);
    }

    #[test]
    fn any_single_correctly_typed_known_key_is_enough() {
        for s in [
            r#"{"folders":[]}"#, // an explicitly empty list is accepted (the UI warns)
            r#"{"folders":[1,2]}"#, // a list of unusable entries too
            r#"{"terminal":"t"}"#,
            r#"{"ai_aliases":""}"#,
            r#"{"blender_path":"b"}"#,
            r#"{"theme":{}}"#,
        ] {
            assert!(parse(s).is_ok(), "{s}");
        }
    }

    #[test]
    fn files_whose_known_keys_all_have_the_wrong_type_are_rejected() {
        let want = "This file does not look like a Blinker UI config (no folders or settings found)";
        for s in [
            r#"{"folders":"x"}"#,
            r#"{"folders":null}"#,
            r#"{"folders":{"a":1}}"#,
            r#"{"folders":"x","terminal":5,"theme":[]}"#,
        ] {
            assert_eq!(parse(s).unwrap_err(), want, "{s}");
        }
        // one correctly typed key rescues it; the wrong-typed folders count as 0
        let p = parse(r#"{"folders":"x","terminal":"t"}"#).unwrap();
        assert_eq!(p.summary.folders, 0);
        assert!(p.config.invalid.contains_key("folders"));
    }

    #[test]
    fn fingerprint_changes_with_any_byte() {
        let a = parse(r#"{"folders":[]}"#).unwrap().summary.fingerprint;
        assert_eq!(a, parse(r#"{"folders":[]}"#).unwrap().summary.fingerprint);
        assert_ne!(a, parse(r#"{"folders":[] }"#).unwrap().summary.fingerprint);
    }

    #[test]
    fn wrong_typed_fields_follow_the_lenient_loader() {
        let p = parse(r#"{"folders":"x","terminal":5,"ai_aliases":"a","blender_path":false,"theme":[]}"#).unwrap();
        assert_eq!(p.summary.folders, 0);
        assert!(!p.summary.has_terminal && !p.summary.has_blender_path && !p.summary.has_theme);
        assert!(p.summary.has_ai_aliases);
        assert_eq!(p.config.invalid.get("terminal"), Some(&serde_json::json!(5)));
        assert_eq!(p.config.invalid.get("folders"), Some(&serde_json::json!("x")));
        assert_eq!(p.config.ai_aliases, "a");
    }

    #[test]
    fn unparsable_folder_entries_are_counted_and_kept() {
        let p = parse(r#"{"folders":[{"path":"a"},{"path":"b","port":99999},"junk",{"path":"c"}]}"#).unwrap();
        assert_eq!(p.summary.folders, 2);
        assert_eq!(p.summary.skipped_folders, 2);
        assert_eq!(p.config.unparsed_folders.len(), 2);
        assert_eq!(p.summary.sample_paths, ["a", "c"]);
    }

    #[test]
    fn unknown_top_level_keys_stay_in_extra() {
        let p = parse(r#"{"folders":[],"launchers":[{"name":"x"}]}"#).unwrap();
        assert_eq!(p.config.extra["launchers"][0]["name"], "x");
    }

    #[test]
    fn size_cap_and_unreadable_paths() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("c.json");
        fs::write(&f, OLD_PYTHON_CONFIG).unwrap();
        assert!(read_config_file_limited(&f, 10_000).is_ok());
        let e = read_config_file_limited(&f, 100).unwrap_err();
        assert_eq!(e, "Config file is too large (limit 100 bytes)");
        // the default cap is 5 MB: a 5 MB + 1 file is refused without being parsed
        let big = t.path().join("big.json");
        fs::write(&big, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert_eq!(read_config_file(&big).unwrap_err(), "Config file is too large (limit 5 MB)");
        let missing = t.path().join("nope.json");
        let e = read_config_file(&missing).unwrap_err();
        assert!(e.starts_with("[Errno 2] No such file or directory: '"), "{e}");
        let e = read_config_file(t.path()).unwrap_err(); // a directory
        assert!(e.starts_with("[Errno 13] Permission denied: '"), "{e}");
    }
}
