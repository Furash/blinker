use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
pub fn find_blender(env_path: Option<&str>) -> Option<String> {
    let roots: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)"]
        .iter()
        .filter_map(|v| std::env::var_os(v))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .collect();
    find_blender_in(env_path, std::env::var_os("PATH"), &roots)
}

fn find_blender_in(
    env_path: Option<&str>,
    path_var: Option<OsString>,
    program_roots: &[PathBuf],
) -> Option<String> {
    if let Some(e) = env_path {
        if !e.is_empty() && Path::new(e).is_file() {
            return Some(e.to_string());
        }
    }
    if let Some(pv) = path_var {
        for dir in std::env::split_paths(&pv) {
            let exe = dir.join("blender.exe");
            if exe.is_file() {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }
    for root in program_roots {
        let bf = root.join("Blender Foundation");
        let Ok(rd) = std::fs::read_dir(&bf) else { continue };
        let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        for d in dirs {
            let exe = d.join("blender.exe");
            if exe.is_file() {
                return Some(exe.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"").unwrap();
    }

    #[test]
    fn env_path_wins_when_file() {
        let t = tempfile::tempdir().unwrap();
        let exe = t.path().join("mine.exe");
        touch(&exe);
        let other = t.path().join("bin");
        touch(&other.join("blender.exe"));
        let got = find_blender_in(exe.to_str(), Some(other.clone().into_os_string()), &[]);
        assert_eq!(got.as_deref(), exe.to_str());
    }

    #[test]
    fn env_path_missing_falls_through_to_path() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        touch(&bin.join("blender.exe"));
        let got = find_blender_in(t.path().join("nope").join("blender.exe").to_str(), Some(bin.clone().into_os_string()), &[]);
        assert_eq!(got.map(PathBuf::from), Some(bin.join("blender.exe")));
    }

    #[test]
    fn program_files_newest_first_then_x86() {
        let t = tempfile::tempdir().unwrap();
        let pf = t.path().join("pf");
        let x86 = t.path().join("x86");
        touch(&pf.join("Blender Foundation").join("Blender 4.2").join("blender.exe"));
        touch(&pf.join("Blender Foundation").join("Blender 4.5").join("blender.exe"));
        touch(&x86.join("Blender Foundation").join("Blender 9.9").join("blender.exe"));
        let got = find_blender_in(None, None, &[pf.clone(), x86.clone()]).unwrap();
        assert_eq!(PathBuf::from(got), pf.join("Blender Foundation").join("Blender 4.5").join("blender.exe"));
        let got = find_blender_in(None, None, &[t.path().join("none"), x86.clone()]).unwrap();
        assert!(got.contains("Blender 9.9"));
    }

    #[test]
    fn nothing_found_is_none() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(find_blender_in(None, Some(t.path().into()), &[t.path().to_path_buf()]), None);
    }
}
