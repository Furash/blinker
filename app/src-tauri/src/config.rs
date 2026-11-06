use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io;
use std::path::Path;

pub const DEFAULT_PORT: u16 = 9876;
pub const DEFAULT_REPO: &str = "blinker";
pub const DEFAULT_AI_ALIASES: &str = "claude, codex";

fn default_port() -> u16 { DEFAULT_PORT }
fn default_repo() -> String { DEFAULT_REPO.to_string() }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    #[serde(default)]
    pub path: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_repo")]
    pub repo: String,
    #[serde(default)]
    pub module: String,
    #[serde(default)]
    pub blend: String,
    #[serde(default)]
    pub favourite: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Folder {
    fn default() -> Self {
        Folder {
            path: String::new(),
            port: DEFAULT_PORT,
            repo: default_repo(),
            module: String::new(),
            blend: String::new(),
            favourite: false,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub folders: Vec<Folder>,
    pub terminal: String,
    pub ai_aliases: String,
    pub blender_path: String,
    pub theme: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
    /// Folder entries that could not be parsed; written back verbatim on save.
    #[serde(skip)]
    pub unparsed_folders: Vec<Value>,
    /// Top-level values with the wrong type; written back on save unless the
    /// typed field was changed from its default in the meantime.
    #[serde(skip)]
    pub invalid: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            folders: Vec::new(),
            terminal: default_terminal(),
            ai_aliases: DEFAULT_AI_ALIASES.to_string(),
            blender_path: String::new(),
            theme: Map::new(),
            extra: Map::new(),
            unparsed_folders: Vec::new(),
            invalid: Map::new(),
        }
    }
}

fn wt_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| d.join("wt.exe").is_file() || d.join("wt").is_file())
}

pub fn default_terminal() -> String {
    if wt_on_path() {
        r#"wt -d "{path}" {cmd}"#.to_string()
    } else {
        r#"start "" /D "{path}" cmd /K "{cmd}""#.to_string()
    }
}

/// Never fails: missing, empty, invalid or hand-edited files fall back to defaults
/// field by field. Unknown keys are preserved in `extra`.
pub fn load(path: &Path) -> Config {
    let mut cfg = Config::default();
    let Ok(bytes) = std::fs::read(path) else { return cfg };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(text) else { return cfg };

    match obj.remove("folders") {
        Some(Value::Array(items)) => {
            for v in items {
                match parse_folder(&v) {
                    Some(f) => cfg.folders.push(f),
                    None => cfg.unparsed_folders.push(v),
                }
            }
        }
        Some(Value::Null) | None => {}
        Some(other) => {
            cfg.invalid.insert("folders".into(), other);
        }
    }
    // Deliberate deviation: an empty terminal falls back to the default (Python's
    // settings dialog never persists an empty terminal, blinker_ui.py:2191).
    match obj.remove("terminal") {
        Some(Value::String(s)) => {
            if !s.trim().is_empty() {
                cfg.terminal = s;
            }
        }
        Some(Value::Null) | None => {}
        Some(other) => {
            cfg.invalid.insert("terminal".into(), other);
        }
    }
    for key in ["ai_aliases", "blender_path"] {
        match obj.remove(key) {
            Some(Value::String(s)) => {
                if key == "ai_aliases" {
                    cfg.ai_aliases = s;
                } else {
                    cfg.blender_path = s;
                }
            }
            Some(Value::Null) | None => {}
            Some(other) => {
                cfg.invalid.insert(key.into(), other);
            }
        }
    }
    match obj.remove("theme") {
        Some(Value::Object(m)) => cfg.theme = m,
        Some(Value::Null) | None => {}
        Some(other) => {
            cfg.invalid.insert("theme".into(), other);
        }
    }
    cfg.extra = obj;
    cfg
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python-style `int(x)` for int / float / numeric string; must fit a u16.
fn coerce_port(v: &Value) -> Option<u16> {
    let n: i64 = match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) => i,
            None => {
                let f = n.as_f64()?;
                if !f.is_finite() {
                    return None;
                }
                f.trunc() as i64
            }
        },
        Value::String(s) => s.trim().parse::<i64>().ok()?,
        Value::Bool(b) => *b as i64,
        _ => return None,
    };
    u16::try_from(n).ok()
}

fn str_field(obj: &mut Map<String, Value>, key: &str) -> Option<String> {
    match obj.remove(key) {
        None | Some(Value::Null) => Some(String::new()),
        Some(Value::String(s)) => Some(s),
        Some(_) => None,
    }
}

/// Loose, Python-like parse. `None` means the entry cannot be represented and
/// the caller preserves it verbatim.
fn parse_folder(v: &Value) -> Option<Folder> {
    let Value::Object(obj) = v else { return None };
    let mut obj = obj.clone();
    let mut f = Folder::default();
    f.path = str_field(&mut obj, "path")?;
    if let Some(p) = obj.remove("port") {
        f.port = coerce_port(&p)?;
    }
    if let Some(r) = obj.remove("repo") {
        f.repo = match r {
            Value::String(s) => s,
            Value::Null => String::new(),
            _ => return None,
        };
    }
    if f.repo.is_empty() {
        f.repo = default_repo(); // Python: `get("repo", "blinker") or "blinker"`
    }
    f.module = str_field(&mut obj, "module")?;
    f.blend = str_field(&mut obj, "blend")?;
    if let Some(fav) = obj.remove("favourite") {
        f.favourite = truthy(&fav);
    }
    f.extra = obj;
    Some(f)
}

/// Pretty JSON (indent 2), written atomically via temp file + rename.
pub fn save(path: &Path, cfg: &Config) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut value = serde_json::to_value(cfg).map_err(io::Error::other)?;
    if let Value::Object(obj) = &mut value {
        if let Some(Value::Array(arr)) = obj.get_mut("folders") {
            arr.extend(cfg.unparsed_folders.iter().cloned());
        }
        let def = Config::default();
        for (k, v) in &cfg.invalid {
            // Only restore the original if the typed field was not changed since load.
            let untouched = match k.as_str() {
                "folders" => cfg.folders.is_empty() && cfg.unparsed_folders.is_empty(),
                "terminal" => cfg.terminal == def.terminal,
                "ai_aliases" => cfg.ai_aliases == def.ai_aliases,
                "blender_path" => cfg.blender_path == def.blender_path,
                "theme" => cfg.theme.is_empty(),
                _ => false,
            };
            if untouched {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let json = serde_json::to_string_pretty(&value).map_err(io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, json)?;
    rename_with_retry(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Replacing the config fails while another process has it open without delete
/// sharing (an editor, a scanner, a script reading it). Python's in-place
/// `write_text` did not care; retry for up to ~2 s before reporting the error.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match std::fs::rename(from, to) {
            // access denied (open without delete sharing) or ERROR_SHARING_VIOLATION
            Err(e)
                if (e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            r => return r,
        }
    }
}

/// Copies `legacy` to `new` once: only if `new` is absent and `legacy` exists.
pub fn migrate_legacy_config(new: &Path, legacy: &Path) -> bool {
    if new.exists() || !legacy.is_file() {
        return false;
    }
    if let Some(dir) = new.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    std::fs::copy(legacy, new).is_ok()
}

/// Favourites first; order otherwise unchanged (stable).
pub fn sorted_folders(mut folders: Vec<Folder>) -> Vec<Folder> {
    folders.sort_by_key(|f| !f.favourite);
    folders
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn d() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    /// A reader that does not share delete access (Python's open(), an editor, a
    /// scanner) blocks the rename over the config for a moment; save waits it out.
    #[cfg(windows)]
    #[test]
    fn save_retries_while_a_reader_holds_the_config() {
        use std::os::windows::fs::OpenOptionsExt;
        let t = d();
        let p = t.path().join("c.json");
        fs::write(&p, "{}").unwrap();
        let held = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&p).unwrap(); // READ|WRITE, no DELETE
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        let cfg = Config { blender_path: "B".into(), ..Config::default() };
        save(&p, &cfg).expect("saved after the reader let go");
        h.join().unwrap();
        assert_eq!(load(&p).blender_path, "B");
        assert!(!t.path().join("c.json.tmp").exists());
    }

    #[test]
    fn load_missing_returns_defaults() {
        let t = d();
        let c = load(&t.path().join("nope.json"));
        assert!(c.folders.is_empty());
        assert_eq!(c.ai_aliases, "claude, codex");
        assert_eq!(c.terminal, default_terminal());
        assert!(c.extra.is_empty());
    }

    #[test]
    fn load_garbage_returns_defaults() {
        let t = d();
        let p = t.path().join("c.json");
        for s in ["", "   ", "{not json", "[1,2]"] {
            fs::write(&p, s).unwrap();
            let c = load(&p);
            assert!(c.folders.is_empty());
            assert_eq!(c.ai_aliases, "claude, codex");
        }
    }

    #[test]
    fn load_strips_bom() {
        let t = d();
        let p = t.path().join("c.json");
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice(br#"{"folders":[{"path":"a"}],"blender_path":"x"}"#);
        fs::write(&p, b).unwrap();
        let c = load(&p);
        assert_eq!(c.folders.len(), 1);
        assert_eq!(c.blender_path, "x");
    }

    #[test]
    fn roundtrip_preserves_unknown_keys() {
        let t = d();
        let p = t.path().join("c.json");
        fs::write(&p, r#"{"future":1,"folders":[{"path":"a","extra_f":true}]}"#).unwrap();
        let c = load(&p);
        save(&p, &c).unwrap();
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["future"], 1);
        assert_eq!(v["folders"][0]["path"], "a");
        assert_eq!(v["folders"][0]["port"], 9876);
        assert_eq!(v["folders"][0]["extra_f"], true);
    }

    #[test]
    fn legacy_config_example_loads() {
        let t = d();
        let p = t.path().join("c.json");
        fs::write(&p, r#"{
  "folders": [
    { "path": "D:/git/Blender_CCP_Tools", "port": 9876, "repo": "blinker", "module": "", "blend": "" }
  ],
  "launchers": [
    { "name": "CLAUDE", "command": "wt -d {path} -NoExit -Command claude" }
  ]
}"#).unwrap();
        let c = load(&p);
        assert_eq!(c.folders.len(), 1);
        assert_eq!(c.folders[0].port, 9876);
        assert!(!c.folders[0].favourite);
        assert_eq!(c.extra["launchers"][0]["name"], "CLAUDE");
        save(&p, &c).unwrap();
        let c2 = load(&p);
        assert_eq!(c2.extra["launchers"][0]["name"], "CLAUDE");
    }

    #[test]
    fn migrate_copies_once_and_never_overwrites() {
        let t = d();
        let legacy = t.path().join("old.json");
        let new = t.path().join("sub").join("new.json");
        fs::write(&legacy, "one").unwrap();
        assert!(migrate_legacy_config(&new, &legacy));
        assert_eq!(fs::read_to_string(&new).unwrap(), "one");
        fs::write(&legacy, "two").unwrap();
        assert!(!migrate_legacy_config(&new, &legacy));
        assert_eq!(fs::read_to_string(&new).unwrap(), "one");
        assert!(!migrate_legacy_config(&t.path().join("x.json"), &t.path().join("missing.json")));
    }

    fn load_str(s: &str) -> (tempfile::TempDir, std::path::PathBuf, Config) {
        let t = d();
        let p = t.path().join("c.json");
        fs::write(&p, s).unwrap();
        let c = load(&p);
        (t, p, c)
    }

    fn reread(p: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn string_port_is_coerced() {
        let (_t, _p, c) = load_str(r#"{"folders":[{"path":"a","port":"9877"}]}"#);
        assert_eq!(c.folders[0].port, 9877);
    }

    #[test]
    fn float_port_is_coerced() {
        let (_t, _p, c) = load_str(r#"{"folders":[{"path":"a","port":9877.0}]}"#);
        assert_eq!(c.folders[0].port, 9877);
    }

    #[test]
    fn truthy_favourite_int_is_coerced() {
        let (_t, _p, c) =
            load_str(r#"{"folders":[{"path":"a","favourite":1},{"path":"b","favourite":0}]}"#);
        assert!(c.folders[0].favourite);
        assert!(!c.folders[1].favourite);
    }

    #[test]
    fn unparseable_folder_entry_survives_save() {
        let (_t, p, c) = load_str(
            r#"{"folders":[{"path":"a"},{"path":"b","port":99999},"junk",{"path":"c","port":"abc"}]}"#,
        );
        assert_eq!(c.folders.len(), 1);
        save(&p, &c).unwrap();
        let v = reread(&p);
        let arr = v["folders"].as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert!(arr.iter().any(|x| x["port"] == 99999));
        assert!(arr.iter().any(|x| x == "junk"));
        assert!(arr.iter().any(|x| x["port"] == "abc"));
    }

    #[test]
    fn wrong_typed_top_level_values_survive_save() {
        let (_t, p, c) =
            load_str(r#"{"theme":[],"terminal":5,"folders":"x","ai_aliases":7,"blender_path":false}"#);
        assert!(c.folders.is_empty());
        assert_eq!(c.terminal, default_terminal());
        save(&p, &c).unwrap();
        let v = reread(&p);
        assert_eq!(v["theme"], serde_json::json!([]));
        assert_eq!(v["terminal"], 5);
        assert_eq!(v["folders"], "x");
        assert_eq!(v["ai_aliases"], 7);
        assert_eq!(v["blender_path"], false);
    }

    #[test]
    fn empty_repo_becomes_blinker() {
        let (_t, _p, c) = load_str(r#"{"folders":[{"path":"a","repo":""}]}"#);
        assert_eq!(c.folders[0].repo, "blinker");
    }

    #[test]
    fn empty_terminal_falls_back_to_default() {
        // Deliberate, benign deviation from Python: its settings dialog never
        // persists an empty terminal (blinker_ui.py:2191), so an empty one is
        // only ever hand-edited and the default is more useful.
        let (_t, _p, c) = load_str(r#"{"terminal":"  "}"#);
        assert_eq!(c.terminal, default_terminal());
    }

    #[test]
    fn favourites_sort_first_stable() {
        let f = |p: &str, fav| Folder { path: p.into(), favourite: fav, ..Folder::default() };
        let out = sorted_folders(vec![f("a", false), f("b", true), f("c", false), f("d", true)]);
        let order: Vec<_> = out.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(order, ["b", "d", "a", "c"]);
    }
}
