//! Self-update: blinker_ui.py "self-update" (`UPDATER_PS1`, `_parse_semver`,
//! `_sha256_hex`, `_find_release_assets`, `_spawn_updater`, 199-504) and the branches of
//! `MainWindow._check_for_updates` (1577-1665), with the Python dialog texts. HTTP is
//! WinHTTP (winhttp.rs) as in Python; SHA-256 is the OS's CNG (BCrypt).
//!
//! Updater script (script.rs, v2): same parameters, zip layout and log as the Python
//! app's script (old installs keep running their own copy from blinker_ui.py). It expands
//! the release zip, takes the zip's first directory (`BlinkerUI/`) as the new install
//! folder, swaps it with the install dir and starts `<install>\<exe leaf name>`
//! (`BlinkerUI.exe`); fail-safe: the previous install stays as `<install>.prev`, user
//! icons are carried forward and any failure after the swap rolls back. The config lives
//! in `%APPDATA%\BlinkerUI` (paths.rs), outside the swapped folder, and is copied to
//! `<config dir>\backups\pre-update-*` (backup.rs) before the updater is started.
//!
//! Debug builds only (see docs/dev-notes.md): `BLINKER_UPDATE_API` replaces the release
//! URL, and while it is set `http://127.0.0.1` URLs are accepted besides `https://`.
//! Without it a debug build answers "Running from source" (Python's `not FROZEN`).

mod apply;
mod backup;
mod check;
mod download;
mod release;
mod script;
mod semver;
mod sha256;
mod spawn;
#[cfg(test)]
mod test_support;
mod url;

pub use apply::*;
pub use backup::*;
pub use check::*;
pub use download::*;
pub use release::*;
pub use script::*;
pub use semver::*;
pub use sha256::*;
pub use spawn::*;
pub use url::*;

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
