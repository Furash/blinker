use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use super::{
    apply_offer, dialog, local_http_ok, spawn_updater_for_self, take_offer, Dialog, Icon, Verified,
};

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
pub(super) fn set_pending(p: Option<PathBuf>) {
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
    use crate::updater::test_support::*;
    use crate::updater::{decision_check, store_offer, Decision, UpdateCheck};

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
}
