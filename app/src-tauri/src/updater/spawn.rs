use std::path::{Path, PathBuf};

use crate::pyerr;

use super::{updater_script_text, SCRIPT_NAME};

/// `str(OSError)` as Python's subprocess shows it on Windows: `[WinError N] text`.
fn win_error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let s = e.to_string();
            let text = s.strip_suffix(&format!(" (os error {n})")).unwrap_or(&s);
            format!("[WinError {n}] {text}")
        }
        None => e.to_string(),
    }
}

/// The PowerShell command line `_spawn_updater` runs.
pub fn updater_args(script: &Path, pid: u32, zip: &Path, install: &Path, exe: &Path) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"]
        .iter()
        .map(Into::into)
        .collect();
    a.push(script.into());
    a.push("-WaitPid".into());
    a.push(pid.to_string().into());
    a.push("-Zip".into());
    a.push(zip.into());
    a.push("-Install".into());
    a.push(install.into());
    a.push("-Exe".into());
    a.push(exe.into());
    a
}

/// `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe` when it exists, else
/// plain `powershell.exe` (PATH lookup).
pub fn powershell_path(system_root: Option<std::ffi::OsString>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    system_root
        .filter(|r| !r.is_empty())
        .map(|r| PathBuf::from(r).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe"))
        .filter(|p| exists(p))
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

/// `_spawn_updater`: write `UPDATER_PS1` (this app's v2 script, script.rs) to `%TEMP%\blinker_updater.ps1` (UTF-8, CRLF:
/// Python's `write_text` on Windows) and start it in the background; it waits for `pid`
/// to exit.
pub fn spawn_updater(zip: &Path, install: &Path, exe: &Path, pid: u32) -> Result<(), String> {
    let script = std::env::temp_dir().join(SCRIPT_NAME);
    std::fs::write(&script, updater_script_text()).map_err(|e| pyerr::os_error(&e, &script))?;
    let mut cmd = std::process::Command::new(powershell_path(std::env::var_os("SystemRoot"), Path::is_file));
    cmd.args(updater_args(&script, pid, zip, install, exe))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Python uses DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP. On Windows 11 (26100,
    // PowerShell 5.1) a powershell.exe started with DETACHED_PROCESS exits with code 0
    // without running the script (Task 12 report), so the script gets a hidden console
    // of its own instead (CREATE_NO_WINDOW); it still outlives this process.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(drop).map_err(|e| win_error(&e))
}

/// Spawn the updater for this process: install dir = the exe's folder (`APP_DIR`).
pub fn spawn_updater_for_self(zip: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| win_error(&e))?;
    let install = exe.parent().ok_or("the executable has no parent directory")?.to_path_buf();
    spawn_updater(zip, &install, &exe, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_is_the_system_one_when_present() {
        let abs = PathBuf::from("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        assert_eq!(powershell_path(Some("C:\\Windows".into()), |p| p == abs), abs);
        assert_eq!(powershell_path(Some("C:\\Windows".into()), |_| false), PathBuf::from("powershell.exe"));
        assert_eq!(powershell_path(None, |_| true), PathBuf::from("powershell.exe"));
        assert_eq!(powershell_path(Some("".into()), |_| true), PathBuf::from("powershell.exe"));
    }

    #[test]
    fn updater_command_line_like_python() {
        let a = updater_args(Path::new("T\\s.ps1"), 42, Path::new("T\\z.zip"), Path::new("C:\\BlinkerUI"), Path::new("C:\\BlinkerUI\\BlinkerUI.exe"));
        let a: Vec<String> = a.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert_eq!(
            a,
            [
                "-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File", "T\\s.ps1", "-WaitPid", "42",
                "-Zip", "T\\z.zip", "-Install", "C:\\BlinkerUI", "-Exe", "C:\\BlinkerUI\\BlinkerUI.exe",
            ]
        );
    }
}
