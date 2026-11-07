use std::ffi::OsString;
use std::io::{self, PipeReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::config::Folder;

#[derive(Debug, Clone, Default)]
pub struct LaunchSpec {
    pub blender: String,
    pub bootstrap: PathBuf,
    pub folder: Folder,
    pub blend: Option<String>,
    /// Test seam: replaces `--python <bootstrap> [blend]`; each entry is appended raw
    /// (unquoted) to the command line. Always `None` in the app.
    #[doc(hidden)]
    pub args_override: Option<Vec<String>>,
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Everything needed to (re)spawn the same Blender with the same environment.
pub(super) struct Plan {
    pub(super) program: String,
    pub(super) bootstrap: PathBuf,
    pub(super) env: Vec<(&'static str, String)>,
    pub(super) args_override: Option<Vec<String>>,
}

/// `Path.resolve()`: absolute, links resolved, without the `\\?\` prefix.
pub(super) fn resolve(p: &Path) -> String {
    let abs = std::fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p))
        .unwrap_or_else(|_| p.to_path_buf());
    display_path(&abs)
}

pub(super) fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

/// `["--python", BOOTSTRAP] + [blend_file]`
fn blender_args(bootstrap: &Path, blend: Option<&str>) -> Vec<OsString> {
    let mut args = vec![OsString::from("--python"), bootstrap.as_os_str().to_owned()];
    if let Some(b) = blend {
        args.push(OsString::from(b));
    }
    args
}

/// Start Blender with stdout and stderr on one pipe (QProcess `MergedChannels`).
pub(super) fn spawn(plan: &Plan, blend: Option<&str>) -> io::Result<(Child, PipeReader)> {
    let (reader, writer) = io::pipe()?;
    let writer_err = writer.try_clone()?;
    let mut cmd = Command::new(&plan.program);
    match &plan.args_override {
        #[cfg(windows)]
        Some(raw) => {
            use std::os::windows::process::CommandExt;
            for a in raw {
                cmd.raw_arg(a);
            }
        }
        #[cfg(not(windows))]
        Some(raw) => {
            cmd.args(raw);
        }
        None => {
            cmd.args(blender_args(&plan.bootstrap, blend));
        }
    }
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null()).stdout(writer).stderr(writer_err);
    // QProcess uses CREATE_NO_WINDOW when the parent has no console: Blender's console
    // stays hidden and its output still goes to our pipe (STARTF_USESTDHANDLES).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd.spawn()?;
    // `cmd` owns our copies of the write end; drop them so the reader sees EOF.
    drop(cmd);
    Ok((child, reader))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blender::test_support::*;
    use std::fs;

    #[test]
    fn path_with_spaces_and_cyrillic_is_passed_intact() {
        let e = env();
        let bat = e.bat(
            "fake blender.bat",
            "@echo off\nchcp 65001>nul\necho ARGS:%*\nset BLINKER\n",
        );
        let boot_dir = e.t.path().join("boot dir Ж");
        fs::create_dir_all(&boot_dir).unwrap();
        let boot = boot_dir.join("bootstrap.py");
        fs::write(&boot, b"#").unwrap();
        let name = "my addon 'q' & (x) Привет";
        let folder = e.addon(name, 9015);
        let blend = e.t.path().join("сцена 'x' & y.blend").to_string_lossy().into_owned();
        let resolved = display_path(&fs::canonicalize(&folder.path).unwrap());
        e.sup
            .launch(LaunchSpec {
                blender: bat.to_string_lossy().into_owned(),
                bootstrap: boot.clone(),
                folder,
                blend: Some(blend.clone()),
                args_override: None,
            })
            .unwrap();
        e.sink.wait_exits(1);
        let out = e.sup.output(9015);
        assert!(out.contains(&format!("BLINKER_ADDON_PATH={resolved}\r\n")), "{out}");
        assert!(out.contains(&format!("BLINKER_MODULE={name}\r\n")), "{out}");
        assert!(out.contains("BLINKER_LEGACY=1\r\n"), "{out}");
        assert!(out.contains("BLINKER_REPO=blinker\r\n"), "{out}");
        let args = out.lines().find(|l| l.starts_with("ARGS:")).expect("ARGS line");
        assert!(
            args.contains(&format!("--python \"{}\" \"{}\"", boot.display(), blend)),
            "{args}"
        );
        assert!(out.contains(&format!("\n  addon:  {resolved}\n")), "{out}");
        assert_eq!(
            blender_args(&boot, Some(&blend)),
            vec![OsString::from("--python"), boot.clone().into_os_string(), OsString::from(&blend)]
        );
        assert_eq!(blender_args(&boot, None).len(), 2);
    }
}
