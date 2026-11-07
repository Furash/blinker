/// The updater script of this app (v2). Same parameters (`-WaitPid -Zip -Install -Exe`),
/// log (`blinker_update.log`), stage folder (`blinker_update_stage`, the zip's first
/// directory is the new install) and failure message box as the Python app's
/// `UPDATER_PS1` (blinker_ui.py 201-285), but the swap is fail-safe:
///
/// - the new exe is checked in the stage BEFORE the install is touched;
/// - user icons are carried forward: files in the old `<Install>\icons\` that the new
///   `icons\` does not have are copied into it (bundled files are never overwritten;
///   junctions and symlinks are not followed);
/// - the previous install is kept as `<Install>.prev` (one copy). An older `.prev` is
///   moved to `.prev-old` and deleted only after the new install is in place and its
///   exe was started;
/// - any failure after the install was renamed rolls back: the (broken) new install is
///   moved aside to `<Install>.failed`, `.prev` is renamed back (and `.prev-old` back to
///   `.prev`), then `.failed` is deleted (best effort). The box tells the user the state
///   (and what to rename by hand if the restore failed); the working exe is started
///   again. A failure to start the new exe counts. `Log` never throws and every rollback
///   step runs on its own, so one failing step does not skip the others.
///
/// Environment (tests, see docs/dev-notes.md): `BLINKER_UPDATE_TEMP` replaces `%TEMP%` for
/// the stage folder and the log; `BLINKER_UPDATE_NO_UI` logs the failure message instead
/// of showing the box; `BLINKER_UPDATE_FORCE_COPY` copies the staged folder into place
/// instead of moving it (the rarely used safety net taken when Move-Item fails before
/// creating anything; on PowerShell 5.1 a cross-volume Move-Item of a folder normally
/// succeeds, copying and deleting by itself). A move that fails part-way is an error
/// (rollback), never completed by copying. Exit code 0 = updated, 1 = failed (rolled
/// back where possible).
///
/// Must stay valid for Windows PowerShell 5.1 (`powershell.exe`); spawn.rs writes it with
/// CRLF line ends.
pub const UPDATER_PS1: &str = r##"
param(
    [int]$WaitPid,
    [string]$Zip,
    [string]$Install,
    [string]$Exe
)
$ErrorActionPreference = "Stop"
$tmp = $env:TEMP
if ($env:BLINKER_UPDATE_TEMP) { $tmp = $env:BLINKER_UPDATE_TEMP }
$log = Join-Path $tmp "blinker_update.log"
# Logging must never throw: it runs on the rollback path.
function Log($msg) {
    try {
        $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $msg
        Add-Content -Path $log -Value $line -Encoding utf8 -ErrorAction Stop
    } catch { }
}
try { "" | Set-Content -Path $log -Encoding utf8 -ErrorAction Stop } catch { }
Log "updater v2 start pid=$PID waitpid=$WaitPid zip=$Zip install=$Install exe=$Exe ps=$($PSVersionTable.PSVersion)"

$leaf = Split-Path -Leaf $Install
$exeLeaf = Split-Path -Leaf $Exe
$prev = "$Install.prev"
$prevOld = "$Install.prev-old"
$failed = "$Install.failed"
$stage = Join-Path $tmp "blinker_update_stage"
$renamed = $false
$movedPrevOld = $false
$restored = $true
$movedAside = $false
$phase = "prepare"

function Wait-ForExit($targetPid, $timeoutMs) {
    $deadline = (Get-Date).AddMilliseconds($timeoutMs)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-Process -Id $targetPid -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep -Milliseconds 200
    }
    return $false
}

function Try-Rename($from, $to, $attempts = 30) {
    for ($i = 0; $i -lt $attempts; $i++) {
        try { Rename-Item -LiteralPath $from -NewName $to -ErrorAction Stop; return } catch {
            if ($i -eq ($attempts - 1)) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
}

function Try-Remove($path, $attempts = 20) {
    for ($i = 0; $i -lt $attempts; $i++) {
        if (-not (Test-Path -LiteralPath $path)) { return }
        try { Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction Stop; return } catch {
            if ($i -eq ($attempts - 1)) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
}

function Is-Link($item) {
    return [bool]($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
}

# Copy files of the old install's icons\ that the new icons\ lacks (user icons).
# Junctions and symlinks are skipped (never followed).
function Copy-UserIcons($oldDir, $newDir) {
    if (-not (Test-Path -LiteralPath $oldDir -PathType Container)) { return }
    if (Is-Link (Get-Item -LiteralPath $oldDir -Force)) { Log "skipping linked folder $oldDir"; return }
    foreach ($c in @(Get-ChildItem -LiteralPath $oldDir -Force)) {
        $dest = Join-Path $newDir $c.Name
        if (Is-Link $c) { Log "skipping link $($c.FullName)"; continue }
        if ($c.PSIsContainer) { Copy-UserIcons $c.FullName $dest; continue }
        if (-not (Test-Path -LiteralPath $dest)) {
            if (-not (Test-Path -LiteralPath $newDir)) { New-Item -ItemType Directory -Path $newDir -Force | Out-Null }
            Copy-Item -LiteralPath $c.FullName -Destination $dest -ErrorAction Stop
            Log "carried icon forward: $dest"
        }
    }
}

function Copy-Staged($from, $to) {
    Copy-Item -LiteralPath $from -Destination $to -Recurse -Force -ErrorAction Stop
    Remove-Item -LiteralPath $from -Recurse -Force -ErrorAction SilentlyContinue
}

# Move the staged folder into place. A cross-volume Move-Item of a folder normally
# succeeds on PowerShell 5.1 (it copies and deletes by itself). Safety net: if the move
# failed before creating the destination, copy instead. If it failed part-way (the
# destination exists, the stage is incomplete), that is an error: the rollback runs, an
# incomplete install is never completed from an incomplete stage.
function Move-Staged($from, $to) {
    if ($env:BLINKER_UPDATE_FORCE_COPY) {
        Log "BLINKER_UPDATE_FORCE_COPY: copying staged -> install"
        Copy-Staged $from $to
        return
    }
    try { Move-Item -LiteralPath $from -Destination $to -ErrorAction Stop } catch {
        $moveErr = $_
        if (Test-Path -LiteralPath $to) { throw "moving the new version into place failed part-way: $moveErr" }
        Log "move failed before creating $to ($moveErr), copying instead"
        Copy-Staged $from $to
    }
}

# Every step on its own: a failing step never skips the following ones.
function Roll-Back {
    if ($renamed) {
        $script:restored = $false
        $aside = $true
        if (Test-Path -LiteralPath $Install) {
            $asideLeaf = $leaf + ".failed"
            if (Test-Path -LiteralPath $failed) {
                try { Remove-Item -LiteralPath $failed -Recurse -Force -ErrorAction Stop } catch { Log "could not remove an older $failed ($_)" }
                if (Test-Path -LiteralPath $failed) { $asideLeaf = $leaf + ".failed-" + (Get-Date -Format "yyyyMMddHHmmss") }
            }
            try {
                Try-Rename $Install $asideLeaf
                $script:failed = Join-Path (Split-Path -Parent $Install) $asideLeaf
                $script:movedAside = $true
                Log "moved the failed new install aside to $script:failed"
            } catch { $aside = $false; Log "could not move the failed new install aside ($_)" }
        }
        if ($aside) {
            try { Try-Rename $prev $leaf; $script:restored = $true; Log "rolled back: previous install restored to $Install" }
            catch { Log "could not rename $prev back to $Install ($_)" }
        }
        if (Test-Path -LiteralPath $script:failed) {
            try { Remove-Item -LiteralPath $script:failed -Recurse -Force -ErrorAction Stop; Log "removed $script:failed" }
            catch { Log "could not remove $script:failed, a harmless leftover ($_)" }
        }
    }
    if ($movedPrevOld -and -not (Test-Path -LiteralPath $prev)) {
        try { Try-Rename $prevOld ($leaf + ".prev"); Log "restored the earlier previous version $prevOld -> $prev" }
        catch { Log "could not rename $prevOld back to $prev ($_)" }
    }
}

try {
    if (-not (Wait-ForExit $WaitPid 20000)) { Log "WARN: pid $WaitPid still alive after 20s" } else { Log "parent exited" }
    Start-Sleep -Milliseconds 500

    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    Log "expanding $Zip -> $stage"
    Expand-Archive -LiteralPath $Zip -DestinationPath $stage -Force

    $sub = Get-ChildItem -LiteralPath $stage -Directory | Select-Object -First 1
    if ($sub) { $src = $sub.FullName } else { $src = $stage }
    Log "staged source: $src"
    $stagedExe = Join-Path $src $exeLeaf
    if (-not (Test-Path -LiteralPath $stagedExe -PathType Leaf)) { throw "the update has no $exeLeaf (looked for $stagedExe); nothing was changed" }

    Copy-UserIcons (Join-Path $Install "icons") (Join-Path $src "icons")

    if (Test-Path -LiteralPath $prevOld) {
        Log "removing leftover $prevOld"
        Try-Remove $prevOld
    }
    $phase = "rename"
    if (Test-Path -LiteralPath $prev) {
        Log "moving earlier previous version $prev -> $prevOld"
        Try-Rename $prev ($leaf + ".prev-old")
        $movedPrevOld = $true
    }
    if (Test-Path -LiteralPath $Install) {
        Log "renaming install -> $prev"
        Try-Rename $Install ($leaf + ".prev")
        $renamed = $true
    }
    $phase = "swap"
    Log "moving staged -> install"
    Move-Staged $src $Install

    $newExe = Join-Path $Install $exeLeaf
    if (-not (Test-Path -LiteralPath $newExe -PathType Leaf)) { throw "new exe not found at $newExe" }
    Log "launching $newExe"
    Start-Process -FilePath $newExe -WorkingDirectory $Install -ErrorAction Stop

    $phase = "done"
    if ($movedPrevOld) { Remove-Item -LiteralPath $prevOld -Recurse -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $Zip -Force -ErrorAction SilentlyContinue
    Log "updater ok; previous version kept in $prev"
    exit 0
} catch {
    $err = $_
    $failedPhase = $phase
    Log "ERROR: $err"
    try { Log $err.ScriptStackTrace } catch { }
    try { Roll-Back } catch { $restored = $false; Log "rollback error ($_)" }
    try { if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction Stop } } catch { Log "could not remove the stage folder ($_)" }
    $note = ""
    try {
        if (-not $renamed) {
            $note = "Nothing was changed; your current version is started again."
        } elseif ($restored) {
            $note = "Your previous version was restored and is started again."
        } elseif (Test-Path -LiteralPath $Install) {
            $note = "Your previous, working version is in $prev, but the failed update in $Install could not be moved away. Close BlinkerUI and any Blender started from it, delete or rename $Install, then rename $prev to $leaf."
        } else {
            $note = "Your previous, working version is in $prev and is started from there. To restore it, close BlinkerUI and rename $prev to $leaf."
        }
        if ($failedPhase -eq "rename") { $note += "`n`nThe install folder could not be renamed: close any Blender started from BlinkerUI and try again." }
        if ($movedAside -and (Test-Path -LiteralPath $failed)) { $note += "`n`nThe failed update was left in $failed; it can be deleted." }
    } catch { Log "note failed ($_)" }
    $msg = "BlinkerUI update failed:`n$err`n`n$note`n`nLog: $log"
    if ($env:BLINKER_UPDATE_NO_UI) {
        Log "message box skipped (BLINKER_UPDATE_NO_UI): $msg"
    } else {
        try {
            Add-Type -AssemblyName System.Windows.Forms
            [System.Windows.Forms.MessageBox]::Show($msg, "BlinkerUI", 0, 16) | Out-Null
        } catch { Log "messagebox failed ($_)" }
    }
    # Relaunch the working install so the user isn't left without an app.
    try {
        $oldDir = $Install
        if ($renamed -and -not $restored) { $oldDir = $prev }
        $oldExe = Join-Path $oldDir $exeLeaf
        if (Test-Path -LiteralPath $oldExe -PathType Leaf) {
            Log "relaunching $oldExe"
            Start-Process -FilePath $oldExe -WorkingDirectory $oldDir
        }
    } catch { Log "relaunch failed ($_)" }
    exit 1
}
"##;

/// The script as written to disk: CRLF line ends (Python's `write_text` on Windows).
pub fn updater_script_text() -> String {
    UPDATER_PS1.replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::sha256_hex;
    use crate::updater::test_support::file_with;

    /// Pins the updater script. Until v0.2.1 this was byte-identical to the Python app's
    /// `UPDATER_PS1` and compared with blinker_ui.py; v2 (fail-safe swap) is this app's own
    /// script, so only the hash is checked. The Python script is unchanged in blinker_ui.py
    /// (old installs keep running it). Update the hash on purpose when changing the script.
    #[test]
    fn updater_ps1_is_pinned() {
        let f = file_with(UPDATER_PS1.as_bytes());
        assert_eq!(sha256_hex(f.path()).unwrap(), "b8f866b03ba639a2af02c2712cc9107ccce7b94d16b604f91defaccaf81561e1");
        assert!(updater_script_text().contains("\r\n") && !updater_script_text().replace("\r\n", "").contains('\n'));
    }

    /// Runs the real script with the real Windows PowerShell against temp folders.
    #[cfg(windows)]
    mod powershell {
        use super::super::*;
        use crate::updater::test_support::stored_zip;
        use crate::updater::{powershell_path, updater_args};
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};

        /// Never a real process id (Windows pids are multiples of 4), so the script does
        /// not wait.
        const NO_PID: u32 = 2_147_483_645;

        struct Fixture {
            /// deleted on drop with retries (see `Drop`)
            dirs: Vec<PathBuf>,
            root: PathBuf,
            /// `<root>\inst`: holds the install and its siblings
            parent: PathBuf,
            install: PathBuf,
            zip: PathBuf,
            temp: PathBuf,
            exe_bytes: Vec<u8>,
        }

        impl Fixture {
            fn sibling(&self, suffix: &str) -> PathBuf {
                self.parent.join(format!("BlinkerUI{suffix}"))
            }
        }

        /// The exe the script relaunches (whoami, cwd = the install inside the temp dir)
        /// may still be running when the test ends, so deleting is retried for up to 5 s;
        /// a failure is only reported, never a panic. `Deny` guards are declared after the
        /// fixture, so their ACLs are restored before this runs.
        impl Drop for Fixture {
            fn drop(&mut self) {
                for d in &self.dirs {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while d.exists() {
                        if std::fs::remove_dir_all(d).is_ok() {
                            break;
                        }
                        if Instant::now() > deadline {
                            eprintln!("could not remove test dir {}", d.display());
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }

        fn whoami() -> Vec<u8> {
            std::fs::read(Path::new(&std::env::var("SystemRoot").unwrap()).join("System32").join("whoami.exe")).unwrap()
        }

        fn read(p: impl AsRef<Path>) -> String {
            std::fs::read_to_string(p).unwrap_or_else(|e| format!("<{e}>"))
        }

        fn write(p: &Path, s: &[u8]) {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        }

        /// `<root>\inst\BlinkerUI` (old install: exe, old.txt, icons\custom.png,
        /// icons\sub\deep.svg, icons\claude.svg), `<root>\inst\BlinkerUI.prev` from an
        /// earlier update, a release zip with `entries`, and `<root>\t` as the script's temp.
        fn fixture(entries: &[(&str, &[u8])]) -> Fixture {
            fixture_in(entries, None)
        }

        /// As `fixture`, with `inst\` under `install_base` (another volume) when given.
        fn fixture_in(entries: &[(&str, &[u8])], install_base: Option<&Path>) -> Fixture {
            let root = tempfile::tempdir().unwrap().keep();
            let mut dirs = vec![root.clone()];
            let parent = match install_base {
                Some(base) => {
                    let b = tempfile::tempdir_in(base).unwrap().keep();
                    dirs.push(b.clone());
                    b.join("inst")
                }
                None => root.join("inst"),
            };
            let exe_bytes = whoami();
            let install = parent.join("BlinkerUI");
            write(&install.join("BlinkerUI.exe"), &exe_bytes);
            write(&install.join("old.txt"), b"old version");
            write(&install.join("icons").join("custom.png"), b"my icon");
            write(&install.join("icons").join("sub").join("deep.svg"), b"my deep icon");
            write(&install.join("icons").join("claude.svg"), b"old bundled");
            write(&parent.join("BlinkerUI.prev").join("older.txt"), b"older version");
            let zip = root.join("dl").join("BlinkerUI-v9.9.9.zip");
            write(&zip, &stored_zip(entries));
            let temp = root.join("t");
            std::fs::create_dir_all(&temp).unwrap();
            Fixture { dirs, root, parent, install, zip, temp, exe_bytes }
        }

        /// Run the script; its exit code (None: killed after 60 s).
        fn run(f: &Fixture, extra_env: &[(&str, &str)]) -> Option<i32> {
            let script = f.root.join("blinker_updater.ps1");
            std::fs::write(&script, updater_script_text()).unwrap();
            let exe = f.install.join("BlinkerUI.exe");
            let mut cmd = std::process::Command::new(powershell_path(std::env::var_os("SystemRoot"), Path::is_file));
            cmd.args(updater_args(&script, NO_PID, &f.zip, &f.install, &exe))
                .env("BLINKER_UPDATE_TEMP", &f.temp)
                .env("BLINKER_UPDATE_NO_UI", "1")
                .env_remove("BLINKER_UPDATE_FORCE_COPY")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            for (k, v) in extra_env {
                cmd.env(k, v);
            }
            let mut child = cmd.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                if let Some(s) = child.try_wait().unwrap() {
                    return s.code();
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        fn log(f: &Fixture) -> String {
            read(f.temp.join("blinker_update.log"))
        }

        fn icacls(args: &[&std::ffi::OsStr]) -> bool {
            std::process::Command::new("icacls")
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        /// An inheritable deny ACE for Everyone on `dir` (affects what is created below it);
        /// removed again on drop, before the temp dir is deleted.
        struct Deny(PathBuf);

        impl Deny {
            fn new(dir: &Path, spec: &str) -> Deny {
                let ace = format!("*S-1-1-0:{spec}");
                assert!(icacls(&[dir.as_os_str(), "/deny".as_ref(), ace.as_ref()]), "icacls /deny {spec}");
                Deny(dir.to_path_buf())
            }
        }

        impl Drop for Deny {
            fn drop(&mut self) {
                let args = [self.0.as_os_str(), "/remove:d".as_ref(), "*S-1-1-0".as_ref(), "/T".as_ref(), "/C".as_ref(), "/Q".as_ref()];
                icacls(&args);
            }
        }

        fn assert_old_install_intact(f: &Fixture) {
            assert_eq!(std::fs::read(f.install.join("BlinkerUI.exe")).unwrap(), f.exe_bytes);
            assert_eq!(read(f.install.join("old.txt")), "old version");
            assert_eq!(read(f.install.join("icons").join("claude.svg")), "old bundled");
            assert_eq!(read(f.install.join("icons").join("custom.png")), "my icon");
            assert!(!f.install.join("new.txt").exists());
            assert_eq!(read(f.sibling(".prev").join("older.txt")), "older version", "earlier .prev in place");
            assert!(!f.sibling(".prev-old").exists());
            assert!(!f.temp.join("blinker_update_stage").exists());
        }

        fn good_zip() -> Vec<(&'static str, Vec<u8>)> {
            vec![
                ("BlinkerUI/", Vec::new()),
                ("BlinkerUI/BlinkerUI.exe", whoami()),
                ("BlinkerUI/new.txt", b"new version".to_vec()),
                ("BlinkerUI/icons/claude.svg", b"new bundled".to_vec()),
                ("BlinkerUI/icons/nested/bundled.svg", b"new nested".to_vec()),
            ]
        }

        fn entries<'a>(v: &'a [(&'static str, Vec<u8>)]) -> Vec<(&'static str, &'a [u8])> {
            v.iter().map(|(n, b)| (*n, b.as_slice())).collect()
        }

        fn assert_updated(f: &Fixture, code: Option<i32>) {
            let log = log(f);
            assert_eq!(code, Some(0), "log:\n{log}");
            assert!(log.contains("updater ok"), "{log}");
            assert_eq!(std::fs::read(f.install.join("BlinkerUI.exe")).unwrap(), f.exe_bytes);
            assert_eq!(read(f.install.join("new.txt")), "new version");
            assert!(!f.install.join("old.txt").exists());
            assert_eq!(read(f.install.join("icons").join("claude.svg")), "new bundled", "bundled icon not overwritten");
            assert_eq!(read(f.install.join("icons").join("nested").join("bundled.svg")), "new nested");
            assert_eq!(read(f.install.join("icons").join("custom.png")), "my icon", "user icon carried forward");
            assert_eq!(read(f.install.join("icons").join("sub").join("deep.svg")), "my deep icon");
            let prev = f.sibling(".prev");
            assert_eq!(read(prev.join("old.txt")), "old version", ".prev holds the old install");
            assert_eq!(read(prev.join("icons").join("custom.png")), "my icon");
            assert!(!prev.join("older.txt").exists(), "only one previous version is kept");
            assert!(!f.sibling(".prev-old").exists());
            assert!(!f.sibling(".failed").exists());
            assert!(!f.temp.join("blinker_update_stage").exists(), "stage removed");
            assert!(!f.zip.exists(), "zip removed");
        }

        #[test]
        fn update_swaps_keeps_prev_and_carries_user_icons_without_following_links() {
            let z = good_zip();
            let f = fixture(&entries(&z));
            // a junction loop in the old icons: must not be followed or copied
            let icons = f.install.join("icons");
            let made = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(icons.join("loop"))
                .arg(&icons)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(made, "mklink /J");
            let code = run(&f, &[]);
            assert_updated(&f, code);
            assert!(!f.install.join("icons").join("loop").exists(), "junction not carried");
            assert!(log(&f).contains("skipping link"), "{}", log(&f));
        }

        #[test]
        fn forced_copy_update_is_complete() {
            let z = good_zip();
            let f = fixture(&entries(&z));
            let code = run(&f, &[("BLINKER_UPDATE_FORCE_COPY", "1")]);
            assert!(log(&f).contains("BLINKER_UPDATE_FORCE_COPY: copying"), "{}", log(&f));
            assert_updated(&f, code);
        }

        #[test]
        fn zip_without_the_exe_changes_nothing() {
            let f = fixture(&[("BlinkerUI/readme.txt", b"no exe here"), ("BlinkerUI/icons/claude.svg", b"new bundled")]);
            let code = run(&f, &[]);
            let log = log(&f);
            assert_eq!(code, Some(1), "log:\n{log}");
            assert!(log.contains("the update has no BlinkerUI.exe"), "{log}");
            assert!(log.contains("Nothing was changed"), "{log}");
            assert!(log.contains("message box skipped"), "{log}");
            assert!(log.contains(&format!("Log: {}", f.temp.join("blinker_update.log").display())), "{log}");
            assert_old_install_intact(&f);
            assert!(!f.install.join("readme.txt").exists());
            assert!(f.zip.exists(), "the zip is kept after a failure");
        }

        #[test]
        fn failure_after_the_swap_rolls_back() {
            // An exe that cannot be started: the swap happens, Start-Process fails.
            let f = fixture(&[("BlinkerUI/BlinkerUI.exe", b"not a program"), ("BlinkerUI/new.txt", b"new version")]);
            let code = run(&f, &[]);
            let log = log(&f);
            assert_eq!(code, Some(1), "log:\n{log}");
            assert!(log.contains("renaming install"), "the swap happened:\n{log}");
            assert!(log.contains("moved the failed new install aside"), "{log}");
            assert!(log.contains("rolled back: previous install restored"), "{log}");
            assert!(log.contains("restored the earlier previous version"), "{log}");
            assert!(log.contains("Your previous version was restored"), "{log}");
            assert_old_install_intact(&f);
            assert!(!f.sibling(".failed").exists(), "broken new install removed");
        }

        #[test]
        fn failed_copy_into_place_rolls_back() {
            let z = good_zip();
            let f = fixture(&entries(&z));
            // folders created below inst\ (the copied install) cannot receive files
            let _deny = Deny::new(&f.parent, "(CI)(IO)(WD)");
            let code = run(&f, &[("BLINKER_UPDATE_FORCE_COPY", "1")]);
            let log = log(&f);
            assert_eq!(code, Some(1), "log:\n{log}");
            assert!(log.contains("BLINKER_UPDATE_FORCE_COPY: copying"), "{log}");
            assert!(log.contains("moved the failed new install aside"), "{log}");
            assert!(log.contains("rolled back: previous install restored"), "{log}");
            assert_old_install_intact(&f);
            assert!(!f.sibling(".failed").exists(), "partial copy removed");
        }

        /// `<crate>\target` when it is on another volume than `%TEMP%` (else None).
        fn other_volume() -> Option<PathBuf> {
            let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
            let drive = |p: &Path| p.to_string_lossy().chars().take(2).collect::<String>().to_ascii_uppercase();
            (target.is_dir() && drive(&target) != drive(&std::env::temp_dir())).then_some(target)
        }

        #[test]
        fn a_move_that_fails_part_way_rolls_back_instead_of_copying() {
            // A cross-volume Move-Item copies and then deletes. Staged files that cannot
            // be deleted make it fail after the destination was created (part-way).
            let Some(base) = other_volume() else {
                eprintln!("SKIP: target dir and %TEMP% are on the same volume (no part-way move)");
                return;
            };
            let z = good_zip();
            let f = fixture_in(&entries(&z), Some(&base));
            let _deny = Deny::new(&f.temp, "(OI)(CI)(IO)(DE,DC)");
            let code = run(&f, &[]);
            let log = log(&f);
            assert_eq!(code, Some(1), "log:\n{log}");
            assert!(log.contains("moving the new version into place failed part-way"), "{log}");
            assert!(!log.contains("copying instead"), "{log}");
            assert!(log.contains("moved the failed new install aside"), "{log}");
            assert!(log.contains("rolled back: previous install restored"), "{log}");
            assert!(log.contains("restored the earlier previous version"), "{log}");
            assert_eq!(std::fs::read(f.install.join("BlinkerUI.exe")).unwrap(), f.exe_bytes);
            assert_eq!(read(f.install.join("old.txt")), "old version");
            assert!(!f.install.join("new.txt").exists());
            assert_eq!(read(f.sibling(".prev").join("older.txt")), "older version");
            assert!(!f.sibling(".prev-old").exists());
            assert!(!f.sibling(".failed").exists(), "broken destination removed");
        }

        #[test]
        fn undeletable_failed_install_is_moved_aside_and_the_old_one_restored() {
            // copied files (and folders) below inst\ cannot be deleted; the new exe cannot start
            let f = fixture(&[("BlinkerUI/BlinkerUI.exe", b"not a program"), ("BlinkerUI/new.txt", b"new version")]);
            let deny = Deny::new(&f.parent, "(OI)(CI)(IO)(DE,DC)");
            let code = run(&f, &[("BLINKER_UPDATE_FORCE_COPY", "1")]);
            let log = log(&f);
            assert_eq!(code, Some(1), "log:\n{log}");
            assert!(log.contains("moved the failed new install aside"), "{log}");
            assert!(log.contains("rolled back: previous install restored"), "{log}");
            assert!(log.contains("a harmless leftover"), "{log}");
            assert!(log.contains("The failed update was left in"), "{log}");
            assert!(log.contains(&format!("relaunching {}", f.install.join("BlinkerUI.exe").display())), "{log}");
            assert_old_install_intact(&f);
            assert_eq!(read(f.sibling(".failed").join("new.txt")), "new version", "locked leftover stays aside");
            drop(deny);
        }

        #[test]
        fn install_that_cannot_be_renamed_changes_nothing_and_says_why() {
            use std::os::windows::fs::OpenOptionsExt;
            let z = good_zip();
            let f = fixture(&entries(&z));
            // a file open inside the install (no delete sharing) blocks renaming the folder,
            // as a running Blender started from it would; Try-Rename gives up after ~15 s
            let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(f.install.join("old.txt")).unwrap();
            let code = run(&f, &[]);
            drop(held);
            let log = log(&f);
            assert_eq!(code, Some(1), "log:\n{log}");
            assert!(log.contains("Nothing was changed"), "{log}");
            assert!(log.contains("close any Blender started from BlinkerUI and try again"), "{log}");
            assert!(log.contains("restored the earlier previous version"), "{log}");
            assert_old_install_intact(&f);
        }

        /// The log held open without any sharing, so every `Log` call fails.
        fn hold_log(f: &Fixture) -> std::fs::File {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .share_mode(0)
                .open(f.temp.join("blinker_update.log"))
                .unwrap()
        }

        #[test]
        fn a_locked_log_does_not_stop_the_update_or_the_rollback() {
            // update: a throwing Log would end the script at its first line
            let z = good_zip();
            let f = fixture(&entries(&z));
            let held = hold_log(&f);
            let code = run(&f, &[]);
            drop(held);
            assert_eq!(std::fs::metadata(f.temp.join("blinker_update.log")).unwrap().len(), 0, "nothing could be logged");
            assert_eq!(code, Some(0));
            assert_eq!(read(f.install.join("new.txt")), "new version");
            assert_eq!(read(f.sibling(".prev").join("old.txt")), "old version");
            // rollback: the same, on the failure path
            let f = fixture(&[("BlinkerUI/BlinkerUI.exe", b"not a program"), ("BlinkerUI/new.txt", b"new version")]);
            let held = hold_log(&f);
            let code = run(&f, &[]);
            drop(held);
            assert_eq!(code, Some(1));
            assert_old_install_intact(&f);
            assert!(!f.sibling(".failed").exists());
        }
    }
}
