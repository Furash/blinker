use serde::Serialize;
use serde_json::Value;

use crate::winhttp::HttpError;

use super::{parse_semver, VERSION};

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

pub(super) fn dialog(title: &'static str, icon: Icon, text: String) -> Dialog {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::UPDATE_API;
    use serde_json::json;

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

    #[test]
    fn expected_hash_takes_first_token() {
        assert_eq!(expected_hash_from_text(" ABC def\n").as_deref(), Some("abc"));
        assert_eq!(expected_hash_from_text("  \r\n"), None);
    }

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
}
