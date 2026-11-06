//! Terminal launchers ("Open AI") and "Open folder".

use std::fmt;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LAUNCH_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
const DRAIN_WAIT: Duration = Duration::from_millis(500);

/// A failed terminal launch. `Display` yields the message Python shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalError {
    pub ai: String,
    pub cmd: String,
    pub err: String,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Failed to launch '{}'.\n\nCommand:\n  {}\n\nError:\n  {}",
            self.ai, self.cmd, self.err
        )
    }
}

pub fn expand_template(template: &str, path: &str, cmd: &str) -> String {
    template.replace("{path}", path).replace("{cmd}", cmd)
}

/// Drain a pipe on its own thread so a long-running child never blocks on a
/// full pipe; the text is delivered once the pipe reaches EOF.
fn drain<R: Read + Send + 'static>(r: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

/// Spawn `cmd` like Python's `Popen(cmd, shell=True)`: `cmd.exe /c "<cmd>"`
/// (Python also wraps the whole command in quotes) with the command line
/// passed raw (no Rust re-quoting), hidden, in `cwd`. Waits up to 2 s.
/// `Ok(Some(child))` = still running (caller owns it).
fn run_shell(ai: &str, cmd: &str, cwd: &str) -> Result<Option<Child>, TerminalError> {
    let fail = |err: String| TerminalError { ai: ai.into(), cmd: cmd.into(), err };
    let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    let mut child = Command::new(comspec)
        .raw_arg(format!("/c \"{cmd}\""))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());

    let deadline = Instant::now() + LAUNCH_WAIT;
    let status = loop {
        match child.try_wait().map_err(|e| fail(e.to_string()))? {
            Some(s) => break s,
            None if Instant::now() >= deadline => return Ok(Some(child)),
            None => thread::sleep(POLL),
        }
    };
    if status.success() {
        return Ok(None);
    }
    // Grandchildren may hold the pipes open, so never wait for EOF unbounded.
    let out = out_rx.recv_timeout(DRAIN_WAIT).unwrap_or_default();
    let err = err_rx.recv_timeout(DRAIN_WAIT).unwrap_or_default();
    let text = [err.trim(), out.trim()]
        .into_iter()
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match status.code() {
            Some(c) => format!("exited with code {c}"),
            None => "exited abnormally".to_string(),
        });
    Err(fail(text))
}

/// Launch `template` (placeholders expanded) through the shell in `path`.
///
/// BLOCKING, but bounded: returns after at most about 2 s. Call it from a
/// worker thread, never from the UI thread. Failure is reported only if the
/// process exits non-zero within 2 s; still running after that counts as
/// success and the process is left alone (reaped by a detached thread).
pub fn open_ai(template: &str, path: &str, ai: &str) -> Result<(), TerminalError> {
    let cmd = expand_template(template, path, ai);
    if let Some(mut child) = run_shell(ai, &cmd, path)? {
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}

/// The folder argument for explorer.exe (quoted by the caller, by hand: Rust's escaping of
/// a trailing backslash confuses Explorer). Config paths use forward slashes (Qt file
/// dialogs, e.g. `D:/git/addon`), and Explorer opens Documents instead of a path written
/// with `/`, so separators become `\`. A drive root becomes `D:\.`.
fn explorer_arg(path: &str) -> String {
    let path = path.replace('/', "\\");
    let trimmed = path.trim_end_matches('\\');
    if trimmed.is_empty() || trimmed.ends_with(':') {
        format!("{trimmed}\\.") // drive root
    } else {
        trimmed.to_string()
    }
}

fn open_folder_with(program: &str, path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).is_dir() {
        return Err(format!("Path not found: {path}"));
    }
    let mut child = Command::new(program)
        .raw_arg(format!("\"{}\"", explorer_arg(path)))
        .spawn()
        .map_err(|e| e.to_string())?;
    // Explorer exits 1 even on success: never inspect the code, just reap.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn open_folder(path: &str) -> Result<(), String> {
    open_folder_with("explorer.exe", path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_replaces_path_and_cmd() {
        assert_eq!(
            expand_template("wt -d \"{path}\" {cmd}", "C:\\a b", "claude"),
            "wt -d \"C:\\a b\" claude"
        );
    }

    #[test]
    fn template_with_cyrillic_path() {
        assert_eq!(
            expand_template("start \"\" /D \"{path}\" cmd /K \"{cmd}\"", "D:\\проект x", "codex"),
            "start \"\" /D \"D:\\проект x\" cmd /K \"codex\""
        );
    }

    #[test]
    fn explorer_arg_uses_backslashes_and_handles_drive_roots() {
        assert_eq!(explorer_arg("D:/git/Blender_CCP_Tools"), "D:\\git\\Blender_CCP_Tools");
        assert_eq!(explorer_arg("D:/git/addon/"), "D:\\git\\addon");
        assert_eq!(explorer_arg("C:\\a b\\c\\"), "C:\\a b\\c");
        assert_eq!(explorer_arg("D:/"), "D:\\.");
        assert_eq!(explorer_arg("D:"), "D:\\.");
        assert_eq!(explorer_arg("//server/share/dir"), "\\\\server\\share\\dir");
    }

    #[test]
    fn open_folder_rejects_missing_dir() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(open_folder(&p), Err(format!("Path not found: {p}")));
    }

    #[test]
    fn open_folder_rejects_file_path() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f.txt");
        std::fs::write(&f, "x").unwrap();
        let p = f.to_string_lossy().into_owned();
        assert_eq!(open_folder(&p), Err(format!("Path not found: {p}")));
    }

    #[test]
    fn open_folder_spawns_program_for_existing_dir() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("папка с пробелом");
        std::fs::create_dir(&sub).unwrap();
        let p = format!("{}\\", sub.display());
        // Harmless stand-in for explorer.exe; its exit code is ignored.
        assert_eq!(open_folder_with("whoami.exe", &p), Ok(()));
        assert!(open_folder_with("definitely-not-a-program.exe", &p).is_err());
    }

    #[test]
    fn open_ai_reports_nonzero_exit() {
        let d = tempfile::tempdir().unwrap();
        let e = open_ai("cmd /C exit 3", &d.path().to_string_lossy(), "claude").unwrap_err();
        assert_eq!(e.err, "exited with code 3");
        assert_eq!(
            e.to_string(),
            "Failed to launch 'claude'.\n\nCommand:\n  cmd /C exit 3\n\nError:\n  exited with code 3"
        );
    }

    #[test]
    fn open_ai_reports_stderr_text() {
        let d = tempfile::tempdir().unwrap();
        let e = open_ai("no_such_cmd_xyz_{cmd}", &d.path().to_string_lossy(), "q").unwrap_err();
        assert!(e.err.contains("no_such_cmd_xyz_q"), "{}", e.err);
    }

    #[test]
    fn open_ai_ok_when_still_running_after_2s() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_string_lossy().into_owned();
        let t = Instant::now();
        let child = run_shell("ping", "ping -n 30 127.0.0.1", &p).unwrap();
        let el = t.elapsed();
        let mut child = child.expect("should still be running");
        assert!(el >= Duration::from_millis(1900) && el < Duration::from_secs(4), "{el:?}");
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn open_ai_returns_ok_via_public_api_when_running() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_string_lossy().into_owned();
        // ping -n 4 lasts ~3 s: still running at 2 s, finishes on its own.
        assert_eq!(open_ai("ping -n 4 127.0.0.1", &p, "ping"), Ok(()));
    }

    #[test]
    fn quotes_survive_with_spaces_and_cyrillic() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("тест папка");
        std::fs::create_dir(&dir).unwrap();
        let bat = d.path().join("check dir.bat");
        std::fs::write(&bat, "@echo off\r\nif exist \"%~1\\\" (exit /b 0) else (exit /b 7)\r\n").unwrap();
        let tpl = format!("\"{}\" \"{{path}}\"", bat.display());
        let p = dir.to_string_lossy().into_owned();
        assert_eq!(open_ai(&tpl, &p, "x"), Ok(()));
        // Sanity: the check really fails for a missing directory argument.
        let bad = format!("\"{}\" \"{}\\missing\"", bat.display(), d.path().display());
        let e = open_ai(&bad, &p, "x").unwrap_err();
        assert_eq!(e.err, "exited with code 7");
    }
}
