//! Self-update: blinker_ui.py "self-update" (`UPDATER_PS1`, `_parse_semver`,
//! `_sha256_hex`, `_find_release_assets`, `_spawn_updater`, 199-504) and the branches of
//! `MainWindow._check_for_updates` (1577-1665), with the Python dialog texts. HTTP is
//! WinHTTP (winhttp.rs) as in Python; SHA-256 is the OS's CNG (BCrypt).
//!
//! Compatibility with installs of the old Python app: the PowerShell script below is
//! byte-for-byte the one those installs write and run. It expands the release zip, takes
//! the zip's first directory (`BlinkerUI/`) as the new install folder, swaps it with the
//! install dir and starts `<install>\<exe leaf name>` (`BlinkerUI.exe`). The config lives
//! in `%APPDATA%\BlinkerUI` (paths.rs), outside the swapped folder.
//!
//! Debug builds only (see docs/dev-notes.md): `BLINKER_UPDATE_API` replaces the release
//! URL, and while it is set `http://127.0.0.1` URLs are accepted besides `https://`.
//! Without it a debug build answers "Running from source" (Python's `not FROZEN`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

use crate::pyerr;
use crate::winhttp::{self, HttpError};

/// `__version__`
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// `UPDATE_API` (`GITHUB_REPO` = `Furash/blinker`)
pub const UPDATE_API: &str = "https://api.github.com/repos/Furash/blinker/releases/latest";
/// Debug-build override of `UPDATE_API` (ignored by release builds).
#[cfg_attr(not(debug_assertions), allow(dead_code))]
pub const UPDATE_API_ENV: &str = "BLINKER_UPDATE_API";

const JSON_TIMEOUT_MS: u32 = 10_000;
const DOWNLOAD_TIMEOUT_MS: u32 = 60_000;
const SCRIPT_NAME: &str = "blinker_updater.ps1";

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

// ---------------------------------------------------------------------------
// pure helpers
// ---------------------------------------------------------------------------

/// `_parse_semver`: strip, drop leading `v`/`V`, cut at `-` then `+`, three dot parts
/// (missing ones 0), each `int()` or 0 when it is not a number.
pub fn parse_semver(s: &str) -> (u32, u32, u32) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split('-').next().unwrap_or("");
    let s = s.split('+').next().unwrap_or("");
    let mut parts = s.split('.').map(py_int).chain(std::iter::repeat(0));
    let mut next = || parts.next().unwrap_or(0);
    (next(), next(), next())
}

/// `int(p)` for a version part, 0 on ValueError. Accepts surrounding whitespace and
/// single underscores between digits; ASCII digits only; saturates at u32::MAX.
fn py_int(p: &str) -> u32 {
    let t = p.trim();
    let ok = !t.is_empty()
        && t.split('_').all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return 0;
    }
    let digits: String = t.chars().filter(|&c| c != '_').collect();
    digits.parse::<u32>().unwrap_or(u32::MAX)
}

/// A release asset: `name` and `browser_download_url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Asset {
    pub name: String,
    pub url: Option<String>,
}

/// `_find_release_assets`: (zip, sha256) by lower-cased name suffix `.zip.sha256` /
/// `.zip`. Like Python's loop, a later match replaces an earlier one.
pub fn find_release_assets(release: &Value) -> (Option<Asset>, Option<Asset>) {
    let (mut zip, mut sha) = (None, None);
    let assets = release.get("assets").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    for a in assets.iter().filter(|a| a.is_object()) {
        let name = a.get("name").and_then(Value::as_str).unwrap_or("");
        let lower = name.to_lowercase();
        let asset = || Asset {
            name: name.to_string(),
            url: a.get("browser_download_url").and_then(Value::as_str).map(str::to_string),
        };
        if lower.ends_with(".zip.sha256") {
            sha = Some(asset());
        } else if lower.ends_with(".zip") {
            zip = Some(asset());
        }
    }
    (zip, sha)
}

/// The hash in a `.sha256` file: `text.strip().split()[0].lower()`; None when the
/// file has no token (Python: IndexError).
pub fn expected_hash_from_text(text: &str) -> Option<String> {
    text.split_whitespace().next().map(str::to_lowercase)
}

/// SHA-256 of `file` matches the hash in `expected` (case-insensitive, first token).
/// Test-only: the download flow hashes once itself (its mismatch message shows both
/// values); this pins the comparison rules used there.
#[cfg(test)]
pub fn verify_sha256(file: &Path, expected: &str) -> bool {
    match (expected_hash_from_text(expected), sha256_hex(file)) {
        (Some(e), Ok(a)) => e == a,
        _ => false,
    }
}

/// `_sha256_hex`: lower-case hex SHA-256 of a file, streamed (1 MiB chunks).
#[cfg(windows)]
pub fn sha256_hex(path: &Path) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|e| pyerr::os_error(&e, path))?;
    sha256_hex_reader(f)
}

/// Lower-case hex SHA-256 of everything `f` yields (also fingerprints Import Config bytes).
#[cfg(windows)]
pub fn sha256_hex_reader(mut f: impl std::io::Read) -> Result<String, String> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Security::Cryptography::*;

    struct Hash(BCRYPT_HASH_HANDLE);
    impl Drop for Hash {
        fn drop(&mut self) {
            unsafe { BCryptDestroyHash(self.0) };
        }
    }
    let mut out = [0u8; 32];
    unsafe {
        let mut h: BCRYPT_HASH_HANDLE = null_mut();
        let st = BCryptCreateHash(BCRYPT_SHA256_ALG_HANDLE, &mut h, null_mut(), 0, null(), 0, 0);
        if st != 0 {
            return Err(format!("BCryptCreateHash failed (status=0x{:08x})", st as u32));
        }
        let h = Hash(h);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let st = BCryptHashData(h.0, buf.as_ptr(), n as u32, 0);
            if st != 0 {
                return Err(format!("BCryptHashData failed (status=0x{:08x})", st as u32));
            }
        }
        let st = BCryptFinishHash(h.0, out.as_mut_ptr(), 32, 0);
        if st != 0 {
            return Err(format!("BCryptFinishHash failed (status=0x{:08x})", st as u32));
        }
    }
    Ok(out.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(not(windows))]
pub fn sha256_hex(_path: &Path) -> Result<String, String> {
    Err("SHA-256 uses Windows CNG".into())
}

#[cfg(not(windows))]
pub fn sha256_hex_reader(_f: impl std::io::Read) -> Result<String, String> {
    Err("SHA-256 uses Windows CNG".into())
}

/// A downloaded asset's local file name: the last path component of the (network
/// supplied) name with characters Windows rejects replaced, never `.`/`..`/empty and
/// never a device name; otherwise `fallback`.
pub fn safe_file_name(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).trim_start();
    if cleaned.is_empty() {
        return fallback.to_string();
    }
    const DEVICES: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1",
        "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = cleaned.split('.').next().unwrap_or("").trim_end();
    if DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem)) {
        return format!("_{cleaned}");
    }
    cleaned.to_string()
}

/// Asset URLs must be `https://`. `local_http_ok` (debug builds with
/// `BLINKER_UPDATE_API` set) also allows `http://127.0.0.1` for the local test server.
/// Decided on the parsed target (what WinHTTP connects to); URLs with user info are
/// refused.
pub fn url_allowed(url: &str, local_http_ok: bool) -> bool {
    match winhttp::split_url(url) {
        Ok(t) if !t.userinfo => t.secure || (local_http_ok && t.host == "127.0.0.1"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// decisions and dialog texts (`_check_for_updates`)
// ---------------------------------------------------------------------------

/// QMessageBox kind for the UI's themed message box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Icon {
    Information,
    Warning,
    Critical,
    Question,
}

/// One message box: title, text, icon. Questions (Yes/No) use `Icon::Question` or,
/// for "Proceed anyway?", `Icon::Warning` (QMessageBox.warning with Yes|No).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dialog {
    pub title: &'static str,
    pub text: String,
    pub icon: Icon,
}

fn dialog(title: &'static str, icon: Icon, text: String) -> Dialog {
    Dialog { title, text, icon }
}

/// What the release JSON says, compared with the running version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// no (or an empty) `tag_name`
    NoTag,
    UpToDate { tag: String },
    NoZipAsset { tag: String },
    Available(Offer),
}

/// An installable release, everything the confirmation and `apply_update` need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Offer {
    pub current: String,
    pub tag: String,
    /// `"\n\n" + notes[:500] + "…"` or empty
    pub notes_preview: String,
    pub zip: Asset,
    pub sha: Option<Asset>,
}

/// The decision part of `_check_for_updates` after the release JSON arrived.
pub fn decide(release: &Value, current: &str) -> Decision {
    // `(release.get(k) or "")`: a missing, null or non-string value counts as empty
    let text = |k: &str| release.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let tag = text("tag_name");
    if tag.is_empty() {
        return Decision::NoTag;
    }
    if parse_semver(&tag) <= parse_semver(current) {
        return Decision::UpToDate { tag };
    }
    let (zip, sha) = find_release_assets(release);
    let Some(zip) = zip else {
        return Decision::NoZipAsset { tag };
    };
    let notes = text("body");
    let notes_preview = if notes.is_empty() {
        String::new()
    } else {
        let head: String = notes.chars().take(500).collect();
        let more = if notes.chars().count() > 500 { "…" } else { "" };
        format!("\n\n{head}{more}")
    };
    Decision::Available(Offer { current: current.to_string(), tag, notes_preview, zip, sha })
}

/// `check_for_updates` result: one of the Python flow's branches with its message box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateCheck {
    /// `not FROZEN` (a debug build without `BLINKER_UPDATE_API`)
    FromSource { dialog: Dialog },
    HttpError { code: u32, dialog: Dialog },
    Error { dialog: Dialog },
    /// no tag, or the tag is not newer
    UpToDate { dialog: Dialog },
    NoZipAsset { tag: String, dialog: Dialog },
    /// the Yes/No "Update available" question; on Yes the UI calls `apply_update`
    Available { offer: Offer, dialog: Dialog },
}

pub fn from_source_check() -> UpdateCheck {
    UpdateCheck::FromSource {
        dialog: dialog(
            "Updates",
            Icon::Information,
            format!("Running from source (v{VERSION}).\nSelf-update only applies to packaged builds."),
        ),
    }
}

/// The message for a failed release request.
pub fn fetch_error_check(e: &HttpError) -> UpdateCheck {
    match e {
        HttpError::Status(code) => UpdateCheck::HttpError {
            code: *code,
            dialog: dialog(
                "Updates",
                Icon::Warning,
                format!("GitHub returned HTTP {code}. Check the repo URL or try later."),
            ),
        },
        HttpError::Other(msg) => UpdateCheck::Error {
            dialog: dialog("Updates", Icon::Warning, format!("Could not check for updates:\n{msg}")),
        },
    }
}

/// The message for a decision (`current` is the running version, without `v`).
pub fn decision_check(d: Decision, current: &str) -> UpdateCheck {
    match d {
        Decision::NoTag | Decision::UpToDate { .. } => UpdateCheck::UpToDate {
            dialog: dialog("Updates", Icon::Information, format!("You are on the latest version (v{current}).")),
        },
        Decision::NoZipAsset { tag } => UpdateCheck::NoZipAsset {
            dialog: dialog("Updates", Icon::Warning, format!("Release {tag} has no .zip asset to install.")),
            tag,
        },
        Decision::Available(offer) => UpdateCheck::Available {
            dialog: dialog(
                "Update available",
                Icon::Question,
                format!(
                    "A new version is available.\n\nCurrent:  v{}\nLatest:   {}\n\n\
                     BlinkerUI will download, close, replace itself, and relaunch.{}",
                    offer.current, offer.tag, offer.notes_preview
                ),
            ),
            offer,
        },
    }
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

/// `BLINKER_UPDATE_API`, honoured only by debug builds.
pub fn api_override() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        std::env::var(UPDATE_API_ENV).ok().filter(|s| !s.is_empty())
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

fn local_http_ok() -> bool {
    api_override().is_some()
}

/// GET the release JSON (`_http_json(UPDATE_API)`), refusing a non-https URL.
pub fn fetch_release(url: &str, local_http_ok: bool) -> Result<Value, HttpError> {
    if !url_allowed(url, local_http_ok) {
        return Err(HttpError::Other(insecure_url(url)));
    }
    winhttp::get_json(url, JSON_TIMEOUT_MS)
}

fn insecure_url(url: &str) -> String {
    format!("refusing a non-https URL: {url}")
}

/// The offer of the last check, applied by `apply` (the webview never chooses what gets
/// downloaded or installed). Every check replaces it.
static OFFER: Mutex<Option<Offer>> = Mutex::new(None);

fn store_offer(c: &UpdateCheck) {
    let offer = match c {
        UpdateCheck::Available { offer, .. } => Some(offer.clone()),
        _ => None,
    };
    *OFFER.lock().unwrap_or_else(|e| e.into_inner()) = offer;
}

fn take_offer() -> Option<Offer> {
    OFFER.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The whole check (blocking): source-run message, request, decision. An available
/// update is kept for `apply`.
pub fn check() -> UpdateCheck {
    let url = match api_override() {
        Some(u) => u,
        None if cfg!(debug_assertions) => return from_source_check(),
        None => UPDATE_API.to_string(),
    };
    let c = check_url(&url, local_http_ok(), VERSION);
    store_offer(&c);
    c
}

/// `check` against an explicit URL and version (also used by the self-test).
pub fn check_url(url: &str, local_http_ok: bool, current: &str) -> UpdateCheck {
    match fetch_release(url, local_http_ok) {
        Err(e) => fetch_error_check(&e),
        Ok(v) if !v.is_object() => {
            // Python: `release.get(...)` on a non-dict raises AttributeError
            fetch_error_check(&HttpError::Other(format!(
                "'{}' object has no attribute 'get'",
                pyerr::json_type_name(&v)
            )))
        }
        Ok(v) => decision_check(decide(&v, current), current),
    }
}

// ---------------------------------------------------------------------------
// download, verify, spawn
// ---------------------------------------------------------------------------

/// Outcome of downloading the zip and checking its SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    /// verified: install this zip
    Ready(PathBuf),
    DownloadFailed(String),
    /// the zip was deleted
    Mismatch { expected: String, actual: String },
    /// the hash could not be checked (the zip is kept until the user decides)
    Unverifiable { zip: PathBuf, error: String },
}

/// Download the zip (and its `.sha256`) into `tmp` and verify it, as
/// `_check_for_updates` after Yes. `sha` = (url, name) of the `.zip.sha256` asset. A
/// missing `browser_download_url` fails as Python's `asset["browser_download_url"]`.
pub fn download_and_verify(
    zip_url: Option<&str>,
    zip_name: &str,
    sha: Option<(Option<&str>, &str)>,
    tmp: &Path,
    local_http_ok: bool,
) -> Verified {
    const NO_URL: &str = "'browser_download_url'"; // str(KeyError)
    let zip_file = safe_file_name(zip_name, "BlinkerUI-update.zip");
    let zip_path = tmp.join(&zip_file);
    let Some(zip_url) = zip_url else {
        return Verified::DownloadFailed(NO_URL.into());
    };
    if !url_allowed(zip_url, local_http_ok) {
        return Verified::DownloadFailed(insecure_url(zip_url));
    }
    if let Err(e) = winhttp::download(zip_url, &zip_path, DOWNLOAD_TIMEOUT_MS) {
        return Verified::DownloadFailed(e.to_string());
    }
    // Python (`if sha_asset is not None:`): no .sha256 asset, no check and no question.
    let Some((sha_url, sha_name)) = sha else {
        return Verified::Ready(zip_path);
    };
    let check = || -> Result<Option<(String, String)>, String> {
        let sha_url = sha_url.ok_or(NO_URL)?;
        if !url_allowed(sha_url, local_http_ok) {
            return Err(insecure_url(sha_url));
        }
        let mut sha_file = safe_file_name(sha_name, &format!("{zip_file}.sha256"));
        if sha_file.eq_ignore_ascii_case(&zip_file) {
            sha_file = format!("{zip_file}.sha256");
        }
        let sha_path = tmp.join(sha_file);
        winhttp::download(sha_url, &sha_path, DOWNLOAD_TIMEOUT_MS).map_err(|e| e.to_string())?;
        let bytes = std::fs::read(&sha_path).map_err(|e| pyerr::os_error(&e, &sha_path));
        let _ = std::fs::remove_file(&sha_path);
        let text = pyerr::decode_text(&bytes?)?;
        let expected = expected_hash_from_text(&text).ok_or("list index out of range")?;
        let actual = sha256_hex(&zip_path)?;
        Ok((expected != actual).then_some((expected, actual)))
    };
    match check() {
        Ok(None) => Verified::Ready(zip_path),
        Ok(Some((expected, actual))) => {
            let _ = std::fs::remove_file(&zip_path);
            Verified::Mismatch { expected, actual }
        }
        Err(error) => Verified::Unverifiable { zip: zip_path, error },
    }
}

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

/// `_spawn_updater`: write `UPDATER_PS1` to `%TEMP%\blinker_updater.ps1` (UTF-8, CRLF:
/// Python's `write_text` on Windows) and start it in the background; it waits for `pid`
/// to exit.
pub fn spawn_updater(zip: &Path, install: &Path, exe: &Path, pid: u32) -> Result<(), String> {
    let script = std::env::temp_dir().join(SCRIPT_NAME);
    std::fs::write(&script, UPDATER_PS1.replace('\n', "\r\n")).map_err(|e| pyerr::os_error(&e, &script))?;
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

/// `apply_update` / `continue_update` result: Started (the app then exits) or the
/// message box to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApplyResult {
    Started,
    DownloadFailed { dialog: Dialog },
    ShaMismatch { expected: String, actual: String, dialog: Dialog },
    /// Yes/No "Proceed anyway?": Yes -> `continue_update(zip_path)`, No -> `cancel_update`
    ShaUnverifiable { zip_path: String, dialog: Dialog },
    LaunchFailed { dialog: Dialog },
}

/// The zip waiting for the user's "Proceed anyway?" answer.
static PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Set the pending zip; a different zip pending before is deleted.
fn set_pending(p: Option<PathBuf>) {
    let old = std::mem::replace(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()), p.clone());
    if let Some(old) = old.filter(|o| Some(o) != p.as_ref()) {
        let _ = std::fs::remove_file(old);
    }
}

/// Take the pending zip if `zip_path` names it (the UI cannot point the updater or the
/// delete at any other file).
pub fn take_pending(zip_path: &str) -> Option<PathBuf> {
    let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_deref().is_some_and(|p| p == Path::new(zip_path)) {
        g.take()
    } else {
        None
    }
}

/// Map a verification outcome to the UI result; a ready zip starts the updater.
pub fn verified_result(v: Verified, spawn: impl FnOnce(&Path) -> Result<(), String>) -> ApplyResult {
    match v {
        Verified::Ready(zip) => launch_result(&zip, spawn),
        Verified::DownloadFailed(e) => ApplyResult::DownloadFailed {
            dialog: dialog("Updates", Icon::Critical, format!("Download failed:\n{e}")),
        },
        Verified::Mismatch { expected, actual } => ApplyResult::ShaMismatch {
            dialog: dialog(
                "Updates",
                Icon::Critical,
                format!("SHA256 mismatch — aborting.\n\nexpected {expected}\nactual   {actual}"),
            ),
            expected,
            actual,
        },
        Verified::Unverifiable { zip, error } => {
            let r = ApplyResult::ShaUnverifiable {
                zip_path: zip.to_string_lossy().into_owned(),
                dialog: dialog("Updates", Icon::Warning, format!("Could not verify SHA256 ({error}).\nProceed anyway?")),
            };
            set_pending(Some(zip));
            r
        }
    }
}

/// Start the updater for a zip; the "Could not launch updater" box on failure.
pub fn launch_result(zip: &Path, spawn: impl FnOnce(&Path) -> Result<(), String>) -> ApplyResult {
    match spawn(zip) {
        Ok(()) => ApplyResult::Started,
        Err(e) => ApplyResult::LaunchFailed {
            dialog: dialog("Updates", Icon::Critical, format!("Could not launch updater:\n{e}")),
        },
    }
}

/// `apply_update` (blocking): download the offer of the last check into `%TEMP%`,
/// verify, start the updater. A zip still waiting for "Proceed anyway?" is deleted.
pub fn apply() -> ApplyResult {
    set_pending(None);
    let Some(offer) = take_offer() else {
        return ApplyResult::DownloadFailed {
            dialog: dialog("Updates", Icon::Critical, "Download failed:\nno update is pending; check for updates first".into()),
        };
    };
    let v = apply_offer(&offer, &std::env::temp_dir(), local_http_ok());
    verified_result(v, spawn_updater_for_self)
}

/// Download and verify an offer's assets.
pub fn apply_offer(offer: &Offer, tmp: &Path, local_http_ok: bool) -> Verified {
    let sha = offer.sha.as_ref().map(|a| (a.url.as_deref(), a.name.as_str()));
    download_and_verify(offer.zip.url.as_deref(), &offer.zip.name, sha, tmp, local_http_ok)
}

/// "Proceed anyway?" -> Yes.
pub fn continue_pending(zip_path: &str) -> ApplyResult {
    match take_pending(zip_path) {
        Some(zip) => launch_result(&zip, spawn_updater_for_self),
        None => ApplyResult::LaunchFailed {
            dialog: dialog("Updates", Icon::Critical, "Could not launch updater:\nno downloaded update is pending".into()),
        },
    }
}

/// "Proceed anyway?" -> No: `zip_path.unlink(missing_ok=True)`.
pub fn cancel_pending(zip_path: &str) {
    if let Some(zip) = take_pending(zip_path) {
        let _ = std::fs::remove_file(zip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ----- semver -----

    #[test]
    fn semver_orders_0_1_2_below_0_2_0() {
        assert!(parse_semver("0.1.2") < parse_semver("0.2.0"));
        assert!(parse_semver("v0.1.10") > parse_semver("v0.1.9"));
        assert!(parse_semver("v0.1.0") <= parse_semver("0.1.0"));
    }

    #[test]
    fn semver_ignores_prefix_and_suffix() {
        assert_eq!(parse_semver("v1.2.3"), (1, 2, 3));
        assert_eq!(parse_semver("V1.2.3-beta.1"), (1, 2, 3));
        assert_eq!(parse_semver("1.2.3+build.7"), (1, 2, 3));
        assert_eq!(parse_semver("  vv2.0-rc1+x "), (2, 0, 0));
        assert_eq!(parse_semver("v0.2"), (0, 2, 0));
        assert_eq!(parse_semver(""), (0, 0, 0));
        assert_eq!(parse_semver("v1.x.3"), (1, 0, 3));
        assert_eq!(parse_semver("1.2.3.4"), (1, 2, 3));
        // int(): surrounding whitespace and digit-separating underscores are accepted
        assert_eq!(parse_semver("1. 2 .1_0"), (1, 2, 10));
        assert_eq!(parse_semver("1._2.3_"), (1, 0, 0));
        assert_eq!(parse_semver("99999999999.1.1"), (u32::MAX, 1, 1));
    }

    // ----- assets -----

    fn asset(name: &str) -> Value {
        json!({ "name": name, "browser_download_url": format!("https://example.com/{name}") })
    }

    fn a(name: &str) -> Asset {
        Asset { name: name.into(), url: Some(format!("https://example.com/{name}")) }
    }

    #[test]
    fn finds_zip_and_sha_assets() {
        let rel = json!({ "assets": [asset("notes.txt"), asset("BlinkerUI-v0.2.0.ZIP"), asset("BlinkerUI-v0.2.0.zip.SHA256")] });
        assert_eq!(find_release_assets(&rel), (Some(a("BlinkerUI-v0.2.0.ZIP")), Some(a("BlinkerUI-v0.2.0.zip.SHA256"))));
        // a later match replaces an earlier one (Python's loop assigns on every match)
        let rel = json!({ "assets": [asset("a.zip"), asset("b.zip"), asset("a.zip.sha256"), asset("b.zip.sha256")] });
        assert_eq!(find_release_assets(&rel), (Some(a("b.zip")), Some(a("b.zip.sha256"))));
    }

    #[test]
    fn missing_sha_asset_returns_none_for_sha() {
        let rel = json!({ "assets": [asset("BlinkerUI-v0.2.0.zip"), asset("x.sha256")] });
        assert_eq!(find_release_assets(&rel), (Some(a("BlinkerUI-v0.2.0.zip")), None));
        assert_eq!(find_release_assets(&json!({})), (None, None));
        assert_eq!(find_release_assets(&json!({ "assets": null })), (None, None));
        // no URL, odd entries
        let rel = json!({ "assets": [{ "name": "x.zip" }, 3, { "name": null }] });
        assert_eq!(find_release_assets(&rel), (Some(Asset { name: "x.zip".into(), url: None }), None));
    }

    // ----- sha256 -----

    fn file_with(bytes: &[u8]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn sha256_hex_reader_known_vector() {
        assert_eq!(
            sha256_hex_reader(&b"abc"[..]).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[cfg(windows)]
    #[test]
    fn sha256_hex_known_vectors() {
        assert_eq!(sha256_hex(file_with(b"abc").path()).unwrap(), ABC);
        assert_eq!(
            sha256_hex(file_with(b"").path()).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let big = vec![b'a'; 3 << 20]; // several 1 MiB chunks
        assert_eq!(sha256_hex(file_with(&big).path()).unwrap().len(), 64);
    }

    #[test]
    fn verify_sha256_rejects_tampered_file() {
        assert!(!verify_sha256(file_with(b"abd").path(), ABC));
        assert!(!verify_sha256(file_with(b"abc").path(), ""));
        assert!(!verify_sha256(Path::new("Z:\\no\\such\\file.zip"), ABC));
    }

    #[test]
    fn verify_sha256_accepts_matching_lowercase_and_uppercase() {
        let f = file_with(b"abc");
        assert!(verify_sha256(f.path(), ABC));
        assert!(verify_sha256(f.path(), &ABC.to_uppercase()));
        assert!(verify_sha256(f.path(), &format!("  {}  BlinkerUI-v0.2.0.zip\r\n", ABC.to_uppercase())));
    }

    #[test]
    fn expected_hash_takes_first_token() {
        assert_eq!(expected_hash_from_text(" ABC def\n").as_deref(), Some("abc"));
        assert_eq!(expected_hash_from_text("  \r\n"), None);
    }

    // ----- names and urls -----

    #[test]
    fn safe_file_name_keeps_plain_names_only() {
        assert_eq!(safe_file_name("BlinkerUI-v0.2.0.zip", "f"), "BlinkerUI-v0.2.0.zip");
        assert_eq!(safe_file_name("..\\..\\Windows\\evil.zip", "f"), "evil.zip");
        assert_eq!(safe_file_name("../x/evil.zip", "f"), "evil.zip");
        assert_eq!(safe_file_name("..", "f"), "f");
        assert_eq!(safe_file_name("", "f"), "f");
        assert_eq!(safe_file_name("dir/", "f"), "f");
        assert_eq!(safe_file_name("C:evil.zip", "f"), "C_evil.zip");
        assert_eq!(safe_file_name("a<b>|?*\".zip", "f"), "a_b_____.zip");
        assert_eq!(safe_file_name("x.zip. . ", "f"), "x.zip");
        assert_eq!(safe_file_name("CON.zip", "f"), "_CON.zip");
        assert_eq!(safe_file_name("nul", "f"), "_nul");
    }

    #[test]
    fn only_https_urls_unless_local_test_server() {
        assert!(url_allowed("https://github.com/x.zip", false));
        assert!(url_allowed("HTTPS://github.com/x.zip", false));
        assert!(!url_allowed("http://github.com/x.zip", false));
        assert!(!url_allowed("http://127.0.0.1:8000/x.zip", false));
        assert!(!url_allowed("file:///C:/x.zip", true));
        assert!(url_allowed("http://127.0.0.1:8000/x.zip", true));
        assert!(url_allowed("http://127.0.0.1/x.zip", true));
        assert!(!url_allowed("http://127.0.0.1.evil.com/x.zip", true));
        assert!(!url_allowed("http://127.0.0.1@evil.com/x.zip", true));
        assert!(!url_allowed("http://github.com/x.zip", true));
        // user info: the prefix "http://127.0.0.1:" would pass a string check
        assert!(!url_allowed("http://127.0.0.1:1@evil.com/x.zip", true));
        assert!(!url_allowed("http://u:p@127.0.0.1:8000/x.zip", true));
        assert!(!url_allowed("https://u@github.com/x.zip", false));
        assert!(!url_allowed("http://localhost:8000/x.zip", true));
    }

    #[test]
    fn powershell_is_the_system_one_when_present() {
        let abs = PathBuf::from("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        assert_eq!(powershell_path(Some("C:\\Windows".into()), |p| p == abs), abs);
        assert_eq!(powershell_path(Some("C:\\Windows".into()), |_| false), PathBuf::from("powershell.exe"));
        assert_eq!(powershell_path(None, |_| true), PathBuf::from("powershell.exe"));
        assert_eq!(powershell_path(Some("".into()), |_| true), PathBuf::from("powershell.exe"));
    }

    // ----- download + verify against a local HTTP server -----

    /// Serves `routes` (path -> body; anything else 404) on 127.0.0.1 until the test ends.
    fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut req = Vec::new();
                let mut b = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match c.read(&mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&b[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("");
                let (status, body) = match routes.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => ("200 OK", body.clone()),
                    None => ("404 Not Found", b"nope".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = c.write_all(head.as_bytes());
                let _ = c.write_all(&body);
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    const ZIP_BYTES: &[u8] = b"PK fake zip";

    fn zip_hash() -> String {
        sha256_hex(file_with(ZIP_BYTES).path()).unwrap()
    }

    fn offer_for(base: &str, sha_path: Option<&str>) -> Offer {
        Offer {
            current: "0.1.0".into(),
            tag: "v0.2.0".into(),
            notes_preview: String::new(),
            zip: Asset { name: "BlinkerUI-v0.2.0.zip".into(), url: Some(format!("{base}/z.zip")) },
            sha: sha_path.map(|p| Asset { name: "BlinkerUI-v0.2.0.zip.sha256".into(), url: Some(format!("{base}{p}")) }),
        }
    }

    #[test]
    fn no_sha_asset_installs_without_asking() {
        let base = serve(vec![("/z.zip", ZIP_BYTES.to_vec())]);
        let tmp = tempfile::tempdir().unwrap();
        let v = apply_offer(&offer_for(&base, None), tmp.path(), true);
        let zip = tmp.path().join("BlinkerUI-v0.2.0.zip");
        assert_eq!(v, Verified::Ready(zip.clone()));
        assert_eq!(std::fs::read(&zip).unwrap(), ZIP_BYTES);
        // ... and the UI result is Started straight away (no "Proceed anyway?")
        assert_eq!(verified_result(v, |_| Ok(())), ApplyResult::Started);
    }

    #[test]
    fn matching_sha_is_ready_and_tampered_sha_deletes_the_zip() {
        let good = format!("{}  BlinkerUI-v0.2.0.zip\n", zip_hash().to_uppercase());
        let base = serve(vec![
            ("/z.zip", ZIP_BYTES.to_vec()),
            ("/good.sha256", good.into_bytes()),
            ("/bad.sha256", "0".repeat(64).into_bytes()),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("BlinkerUI-v0.2.0.zip");
        assert_eq!(apply_offer(&offer_for(&base, Some("/good.sha256")), tmp.path(), true), Verified::Ready(zip.clone()));
        assert!(!tmp.path().join("BlinkerUI-v0.2.0.zip.sha256").exists(), "sha file removed after reading");
        let v = apply_offer(&offer_for(&base, Some("/bad.sha256")), tmp.path(), true);
        assert_eq!(v, Verified::Mismatch { expected: "0".repeat(64), actual: zip_hash() });
        assert!(!zip.exists(), "zip deleted on mismatch");
    }

    #[test]
    fn sha_download_failure_asks_proceed_anyway() {
        let base = serve(vec![("/z.zip", ZIP_BYTES.to_vec()), ("/empty.sha256", Vec::new())]);
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("BlinkerUI-v0.2.0.zip");
        let v = apply_offer(&offer_for(&base, Some("/missing.sha256")), tmp.path(), true);
        assert_eq!(v, Verified::Unverifiable { zip: zip.clone(), error: "HTTP 404".into() });
        assert!(zip.exists(), "zip kept until the user answers");
        let v = apply_offer(&offer_for(&base, Some("/empty.sha256")), tmp.path(), true);
        assert_eq!(v, Verified::Unverifiable { zip, error: "list index out of range".into() });
    }

    #[test]
    fn zip_download_failure_and_insecure_urls() {
        let base = serve(vec![]);
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(apply_offer(&offer_for(&base, None), tmp.path(), true), Verified::DownloadFailed("HTTP 404".into()));
        assert!(!tmp.path().join("BlinkerUI-v0.2.0.zip").exists(), "partial download removed");
        assert_eq!(
            apply_offer(&offer_for(&base, None), tmp.path(), false),
            Verified::DownloadFailed(format!("refusing a non-https URL: {base}/z.zip"))
        );
    }

    /// Tests that touch the process-wide PENDING / OFFER state run one at a time.
    static GLOBALS: Mutex<()> = Mutex::new(());

    #[test]
    fn apply_without_a_checked_offer_is_an_error_and_check_replaces_the_offer() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        store_offer(&UpdateCheck::UpToDate { dialog: dialog("Updates", Icon::Information, String::new()) });
        let ApplyResult::DownloadFailed { dialog: dg } = apply() else { panic!() };
        assert_eq!(dg.text, "Download failed:\nno update is pending; check for updates first");
        let o = offer_for("https://example.com", None);
        store_offer(&decision_check(Decision::Available(o.clone()), "0.1.0"));
        assert_eq!(take_offer(), Some(o));
        assert_eq!(take_offer(), None);
    }

    #[test]
    fn a_new_pending_zip_deletes_the_old_one() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a.zip"), tmp.path().join("b.zip"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        set_pending(Some(a.clone()));
        set_pending(Some(a.clone())); // the same zip again is kept
        assert!(a.exists());
        set_pending(Some(b.clone()));
        assert!(!a.exists() && b.exists());
        set_pending(None);
        assert!(!b.exists());
    }

    // ----- decisions -----

    fn release(tag: Value, body: &str, assets: Vec<Value>) -> Value {
        json!({ "tag_name": tag, "body": body, "assets": assets })
    }

    #[test]
    fn decide_no_tag_or_not_newer_is_up_to_date() {
        assert_eq!(decide(&json!({}), "0.1.0"), Decision::NoTag);
        assert_eq!(decide(&release(json!("  "), "", vec![]), "0.1.0"), Decision::NoTag);
        assert_eq!(decide(&release(Value::Null, "", vec![]), "0.1.0"), Decision::NoTag);
        assert_eq!(decide(&release(json!("v0.1.0"), "", vec![]), "0.1.0"), Decision::UpToDate { tag: "v0.1.0".into() });
        assert_eq!(decide(&release(json!("v0.0.9"), "", vec![]), "0.1.0"), Decision::UpToDate { tag: "v0.0.9".into() });
        let c = decision_check(Decision::NoTag, "0.1.0");
        assert_eq!(
            c,
            UpdateCheck::UpToDate {
                dialog: Dialog { title: "Updates", text: "You are on the latest version (v0.1.0).".into(), icon: Icon::Information }
            }
        );
    }

    #[test]
    fn decide_newer_without_zip_is_no_zip_asset() {
        let d = decide(&release(json!(" v0.2.0 "), "", vec![asset("x.zip.sha256")]), "0.1.0");
        assert_eq!(d, Decision::NoZipAsset { tag: "v0.2.0".into() });
        let UpdateCheck::NoZipAsset { dialog: dg, .. } = decision_check(d, "0.1.0") else { panic!() };
        assert_eq!((dg.title, dg.icon), ("Updates", Icon::Warning));
        assert_eq!(dg.text, "Release v0.2.0 has no .zip asset to install.");
    }

    #[test]
    fn decide_newer_with_zip_is_available_with_notes_preview() {
        let rel = release(json!("v0.2.0"), "\n  Fixes.\n", vec![asset("BlinkerUI-v0.2.0.zip"), asset("BlinkerUI-v0.2.0.zip.sha256")]);
        let d = decide(&rel, "0.1.0");
        let Decision::Available(offer) = &d else { panic!("{d:?}") };
        assert_eq!(offer.notes_preview, "\n\nFixes.");
        assert_eq!(offer.zip, a("BlinkerUI-v0.2.0.zip"));
        assert_eq!(offer.sha, Some(a("BlinkerUI-v0.2.0.zip.sha256")));
        let UpdateCheck::Available { dialog: dg, .. } = decision_check(d, "0.1.0") else { panic!() };
        assert_eq!((dg.title, dg.icon), ("Update available", Icon::Question));
        assert_eq!(
            dg.text,
            "A new version is available.\n\nCurrent:  v0.1.0\nLatest:   v0.2.0\n\n\
             BlinkerUI will download, close, replace itself, and relaunch.\n\nFixes."
        );
    }

    #[test]
    fn notes_preview_is_500_chars_plus_ellipsis() {
        let long = "é".repeat(501);
        let d = decide(&release(json!("v9.0.0"), &long, vec![asset("a.zip")]), "0.1.0");
        let Decision::Available(o) = d else { panic!() };
        assert_eq!(o.notes_preview, format!("\n\n{}…", "é".repeat(500)));
        let d = decide(&release(json!("v9.0.0"), &"x".repeat(500), vec![asset("a.zip")]), "0.1.0");
        let Decision::Available(o) = d else { panic!() };
        assert_eq!(o.notes_preview, format!("\n\n{}", "x".repeat(500)));
        let d = decide(&json!({ "tag_name": "v9", "assets": [asset("a.zip")] }), "0.1.0");
        let Decision::Available(o) = d else { panic!() };
        assert_eq!(o.notes_preview, "");
    }

    #[test]
    fn fetch_errors_and_source_run_messages() {
        let UpdateCheck::HttpError { code, dialog: dg } = fetch_error_check(&HttpError::Status(404)) else { panic!() };
        assert_eq!(code, 404);
        assert_eq!(dg.text, "GitHub returned HTTP 404. Check the repo URL or try later.");
        assert_eq!(dg.icon, Icon::Warning);
        let UpdateCheck::Error { dialog: dg } = fetch_error_check(&HttpError::Other("WinHttpSendRequest failed (err=12007)".into())) else {
            panic!()
        };
        assert_eq!(dg.text, "Could not check for updates:\nWinHttpSendRequest failed (err=12007)");
        let UpdateCheck::FromSource { dialog: dg } = from_source_check() else { panic!() };
        assert_eq!(dg.text, format!("Running from source (v{VERSION}).\nSelf-update only applies to packaged builds."));
        assert_eq!(UPDATE_API, "https://api.github.com/repos/Furash/blinker/releases/latest");
    }

    #[test]
    fn check_refuses_plain_http_release_url() {
        let c = check_url("http://example.com/latest", false, "0.1.0");
        let UpdateCheck::Error { dialog: dg } = c else { panic!("{c:?}") };
        assert_eq!(dg.text, "Could not check for updates:\nrefusing a non-https URL: http://example.com/latest");
    }

    #[test]
    fn apply_results_carry_python_texts() {
        let r = verified_result(Verified::DownloadFailed("HTTP 404".into()), |_| Ok(()));
        assert_eq!(
            r,
            ApplyResult::DownloadFailed { dialog: Dialog { title: "Updates", text: "Download failed:\nHTTP 404".into(), icon: Icon::Critical } }
        );
        let r = verified_result(Verified::Mismatch { expected: "aa".into(), actual: "bb".into() }, |_| Ok(()));
        let ApplyResult::ShaMismatch { dialog: dg, .. } = r else { panic!() };
        assert_eq!(dg.text, "SHA256 mismatch — aborting.\n\nexpected aa\nactual   bb");
        let r = verified_result(Verified::Ready("z.zip".into()), |_| Err("[WinError 2] nope".into()));
        let ApplyResult::LaunchFailed { dialog: dg } = r else { panic!() };
        assert_eq!(dg.text, "Could not launch updater:\n[WinError 2] nope");
        assert_eq!(verified_result(Verified::Ready("z.zip".into()), |_| Ok(())), ApplyResult::Started);
    }

    #[test]
    fn unverifiable_asks_and_only_the_pending_zip_continues() {
        let _g = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        let r = verified_result(Verified::Unverifiable { zip: "C:\\t\\u.zip".into(), error: "HTTP 404".into() }, |_| Ok(()));
        let ApplyResult::ShaUnverifiable { zip_path, dialog: dg } = r else { panic!() };
        assert_eq!(dg.text, "Could not verify SHA256 (HTTP 404).\nProceed anyway?");
        assert_eq!(dg.icon, Icon::Warning);
        assert_eq!(take_pending("C:\\t\\other.zip"), None);
        assert_eq!(take_pending(&zip_path), Some(PathBuf::from("C:\\t\\u.zip")));
        assert_eq!(take_pending(&zip_path), None);
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

