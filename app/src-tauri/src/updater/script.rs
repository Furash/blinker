/// `UPDATER_PS1` (blinker_ui.py 201-285), unchanged: old installs run this same script.
pub const UPDATER_PS1: &str = r##"
param(
    [int]$WaitPid,
    [string]$Zip,
    [string]$Install,
    [string]$Exe
)
$ErrorActionPreference = "Stop"
$log = Join-Path $env:TEMP "blinker_update.log"
function Log($msg) {
    $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $msg
    Add-Content -Path $log -Value $line -Encoding utf8
}
"" | Set-Content -Path $log -Encoding utf8
Log "updater start pid=$PID waitpid=$WaitPid zip=$Zip install=$Install exe=$Exe ps=$($PSVersionTable.PSVersion)"

$leaf = Split-Path -Leaf $Install
$parent = Split-Path -Parent $Install
$bak  = "$Install.bak"

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
        try { Rename-Item -Path $from -NewName $to -ErrorAction Stop; return } catch {
            if ($i -eq ($attempts - 1)) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
}

try {
    if (-not (Wait-ForExit $WaitPid 20000)) { Log "WARN: pid $WaitPid still alive after 20s" } else { Log "parent exited" }
    Start-Sleep -Milliseconds 500

    $stage = Join-Path $env:TEMP "blinker_update_stage"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    Log "expanding $Zip -> $stage"
    Expand-Archive -Path $Zip -DestinationPath $stage -Force

    $sub = Get-ChildItem -Path $stage -Directory | Select-Object -First 1
    if ($sub) { $src = $sub.FullName } else { $src = $stage }
    Log "staged source: $src"

    if (Test-Path $bak) { Remove-Item -Recurse -Force $bak }
    if (Test-Path $Install) {
        Log "renaming install -> bak"
        Try-Rename $Install ($leaf + ".bak")
    }
    Log "moving staged -> install"
    Move-Item -Path $src -Destination $Install

    $newExe = Join-Path $Install (Split-Path -Leaf $Exe)
    Log "launching $newExe"
    if (-not (Test-Path $newExe)) { throw "new exe not found at $newExe" }
    Start-Process -FilePath $newExe -WorkingDirectory $Install

    Remove-Item -Recurse -Force $bak   -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    Remove-Item -Force $Zip            -ErrorAction SilentlyContinue
    Log "updater ok"
} catch {
    Log "ERROR: $_"
    Log $_.ScriptStackTrace
    if ((Test-Path $bak) -and -not (Test-Path $Install)) {
        try { Rename-Item -Path $bak -NewName $leaf; Log "rolled back from bak" } catch { Log "rollback failed: $_" }
    }
    try {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.MessageBox]::Show("BlinkerUI update failed:`n$_`n`nLog: $log", "BlinkerUI", 0, 16) | Out-Null
    } catch { Log "messagebox failed: $_" }
    # On failure, try to relaunch the existing install so the user isn't left without an app.
    try {
        $oldExe = Join-Path $Install (Split-Path -Leaf $Exe)
        if (Test-Path $oldExe) { Start-Process -FilePath $oldExe -WorkingDirectory $Install }
    } catch { Log "relaunch failed: $_" }
}
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::sha256_hex;
    use crate::updater::test_support::file_with;
    use std::path::Path;

    /// `UPDATER_PS1` is the script old installs run. Pinned by hash (stays valid when the
    /// Python app is gone) and, while blinker_ui.py exists, compared with its constant.
    #[test]
    fn updater_ps1_is_byte_identical_to_python() {
        let f = file_with(UPDATER_PS1.as_bytes());
        assert_eq!(sha256_hex(f.path()).unwrap(), "56b23b64522fc7a4131d82bf538f5bb780886a1adf22032c4b20f070960738b6");
        let py = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blinker_ui.py");
        if let Ok(src) = std::fs::read_to_string(py) {
            let src = src.replace("\r\n", "\n");
            let start = src.find("UPDATER_PS1 = r'''").expect("UPDATER_PS1 in blinker_ui.py") + "UPDATER_PS1 = r'''".len();
            let len = src[start..].find("'''").unwrap();
            assert_eq!(UPDATER_PS1, &src[start..start + len]);
        }
    }
}
