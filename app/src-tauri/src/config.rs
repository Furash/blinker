use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io;
use std::path::Path;

pub const DEFAULT_PORT: u16 = 9876;
pub const DEFAULT_REPO: &str = "blinker";
const DEFAULT_AI_ALIASES: &str = "claude, codex";

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
#[allow(dead_code)]
pub fn load(path: &Path) -> Config {
    let mut cfg = Config::default();
    let Ok(bytes) = std::fs::read(path) else { return cfg };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(text) else { return cfg };

    if let Some(Value::Array(items)) = obj.remove("folders") {
        cfg.folders = items
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Folder>(v).ok())
            .collect();
    }
    if let Some(Value::String(s)) = obj.remove("terminal") {
        if !s.trim().is_empty() {
            cfg.terminal = s;
        }
    }
    if let Some(Value::String(s)) = obj.remove("ai_aliases") {
        cfg.ai_aliases = s;
    }
    if let Some(Value::String(s)) = obj.remove("blender_path") {
        cfg.blender_path = s;
    }
    if let Some(Value::Object(m)) = obj.remove("theme") {
        cfg.theme = m;
    }
    cfg.extra = obj;
    cfg
}

/// Pretty JSON (indent 2), written atomically via temp file + rename.
#[allow(dead_code)]
pub fn save(path: &Path, cfg: &Config) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Copies `legacy` to `new` once: only if `new` is absent and `legacy` exists.
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn sorted_folders(mut folders: Vec<Folder>) -> Vec<Folder> {
    folders.sort_by_key(|f| !f.favourite);
    folders
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn d() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

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

    #[test]
    fn favourites_sort_first_stable() {
        let f = |p: &str, fav| Folder { path: p.into(), favourite: fav, ..Folder::default() };
        let out = sorted_folders(vec![f("a", false), f("b", true), f("c", false), f("d", true)]);
        let order: Vec<_> = out.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(order, ["b", "d", "a", "c"]);
    }
}
