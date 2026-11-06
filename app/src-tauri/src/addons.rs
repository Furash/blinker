use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const SCAN_SKIP_DIRS: &[&str] = &[
    ".git", ".github", ".vscode", ".idea", "__pycache__", "node_modules",
    "build", "dist", ".venv", "venv", "env", ".tox", ".pytest_cache",
    ".mypy_cache", ".ruff_cache", "htmlcov", "site-packages", "target",
    "bin", "obj", "out", "templates", "templates_toml", "examples",
    "samples", "tests", "test", "docs", "doc",
];

fn bl_info_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^\s*bl_info\s*=\s*\{").unwrap())
}

fn template_patterns() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        [
            r#"\bid\s*=\s*"ADDON_ID""#,
            r#"\bname\s*=\s*"ADDON_NAME""#,
            r#"\bid\s*=\s*"my_example_extension""#,
            r"(?i)#\s*Example of manifest file",
            r#"\bmaintainer\s*=\s*"AUTHOR_NAME""#,
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

fn read_lossy(p: &Path) -> Option<String> {
    std::fs::read(p).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AddonKind {
    Extension,
    Legacy,
}

#[allow(dead_code)]
pub fn addon_kind(dir: &Path) -> Option<AddonKind> {
    let manifest = dir.join("blender_manifest.toml");
    if manifest.is_file() {
        let text = read_lossy(&manifest).unwrap_or_default();
        if template_patterns().iter().any(|p| p.is_match(&text)) {
            return None;
        }
        return Some(AddonKind::Extension);
    }
    let init = dir.join("__init__.py");
    if init.is_file() {
        if let Some(text) = read_lossy(&init) {
            if bl_info_re().is_match(&text) {
                return Some(AddonKind::Legacy);
            }
        }
    }
    None
}

#[allow(dead_code)]
pub fn find_addons(root: &Path, max_depth: usize) -> Vec<(PathBuf, AddonKind)> {
    fn walk(p: &Path, depth: usize, max_depth: usize, out: &mut Vec<(PathBuf, AddonKind)>) {
        if depth > max_depth {
            return;
        }
        let Ok(rd) = std::fs::read_dir(p) else { return };
        let mut kids: Vec<(String, PathBuf)> = rd
            .filter_map(|e| e.ok())
            .map(|e| (e.file_name().to_string_lossy().to_lowercase(), e.path()))
            .collect();
        kids.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, child) in kids {
            if !child.is_dir() {
                continue;
            }
            let name = child.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || SCAN_SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            if let Some(kind) = addon_kind(&child) {
                out.push((child, kind));
                continue;
            }
            walk(&child, depth + 1, max_depth, out);
        }
    }
    let mut out = Vec::new();
    walk(root, 1, max_depth, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ext(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("blender_manifest.toml"), "id = \"real\"\n").unwrap();
    }
    fn legacy(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("__init__.py"), "bl_info = {\n 'name': 'x'\n}\n").unwrap();
    }
    fn names(r: &[(PathBuf, AddonKind)]) -> Vec<String> {
        r.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn manifest_dir_is_extension() {
        let t = tempfile::tempdir().unwrap();
        ext(t.path());
        assert_eq!(addon_kind(t.path()), Some(AddonKind::Extension));
    }

    #[test]
    fn init_with_bl_info_is_legacy() {
        let t = tempfile::tempdir().unwrap();
        legacy(t.path());
        assert_eq!(addon_kind(t.path()), Some(AddonKind::Legacy));
        fs::write(t.path().join("__init__.py"), "# bl_info = {\nx=1\n").unwrap();
        assert_eq!(addon_kind(t.path()), None);
    }

    #[test]
    fn template_manifest_is_rejected() {
        let t = tempfile::tempdir().unwrap();
        for body in [
            "id = \"ADDON_ID\"",
            "name = \"ADDON_NAME\"",
            "id = \"my_example_extension\"",
            "# example of MANIFEST file",
            "maintainer = \"AUTHOR_NAME\"",
        ] {
            fs::write(t.path().join("blender_manifest.toml"), body).unwrap();
            assert_eq!(addon_kind(t.path()), None, "{body}");
        }
    }

    #[test]
    fn scan_skips_dot_and_skip_dirs() {
        let t = tempfile::tempdir().unwrap();
        for n in ["node_modules", "tests", "docs", ".git", ".hidden", "keep"] {
            ext(&t.path().join(n));
        }
        assert_eq!(names(&find_addons(t.path(), 3)), vec!["keep"]);
    }

    #[test]
    fn scan_does_not_recurse_into_found_addon() {
        let t = tempfile::tempdir().unwrap();
        ext(&t.path().join("outer"));
        ext(&t.path().join("outer").join("inner"));
        assert_eq!(names(&find_addons(t.path(), 3)), vec!["outer"]);
    }

    #[test]
    fn scan_respects_max_depth() {
        let t = tempfile::tempdir().unwrap();
        ext(&t.path().join("a").join("b").join("c"));
        assert_eq!(find_addons(t.path(), 3).len(), 1);
        assert_eq!(find_addons(t.path(), 2).len(), 0);
    }

    #[test]
    fn scan_is_sorted_case_insensitively() {
        let t = tempfile::tempdir().unwrap();
        for n in ["b", "A", "c", "B2"] {
            legacy(&t.path().join(n));
        }
        assert_eq!(names(&find_addons(t.path(), 3)), vec!["A", "b", "B2", "c"]);
    }

    #[test]
    fn non_ascii_and_spaces_scan() {
        let t = tempfile::tempdir().unwrap();
        ext(&t.path().join("Мой аддон"));
        legacy(&t.path().join("папка").join("my addon ü"));
        let r = find_addons(t.path(), 3);
        assert_eq!(names(&r), vec!["Мой аддон", "my addon ü"]);
        assert_eq!(r[1].1, AddonKind::Legacy);
    }

    #[test]
    fn unreadable_root_is_empty() {
        let t = tempfile::tempdir().unwrap();
        assert!(find_addons(&t.path().join("missing"), 3).is_empty());
    }

    #[test]
    fn serialises_lowercase() {
        assert_eq!(serde_json::to_string(&AddonKind::Extension).unwrap(), "\"extension\"");
        assert_eq!(serde_json::to_string(&AddonKind::Legacy).unwrap(), "\"legacy\"");
    }
}
