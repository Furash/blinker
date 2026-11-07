use std::path::{Path, PathBuf};

use crate::pyerr;
use crate::winhttp;

use super::{expected_hash_from_text, insecure_url, safe_file_name, sha256_hex, url_allowed, Offer, DOWNLOAD_TIMEOUT_MS};

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

/// Download and verify an offer's assets.
pub fn apply_offer(offer: &Offer, tmp: &Path, local_http_ok: bool) -> Verified {
    let sha = offer.sha.as_ref().map(|a| (a.url.as_deref(), a.name.as_str()));
    download_and_verify(offer.zip.url.as_deref(), &offer.zip.name, sha, tmp, local_http_ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::test_support::*;
    use crate::updater::{verified_result, ApplyResult};

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
}
