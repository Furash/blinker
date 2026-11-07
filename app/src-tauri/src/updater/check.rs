use std::sync::Mutex;

use serde_json::Value;

use crate::pyerr;
use crate::winhttp::{self, HttpError};

use super::{
    decide, decision_check, fetch_error_check, from_source_check, insecure_url, url_allowed, UpdateCheck, Offer,
    JSON_TIMEOUT_MS, UPDATE_API, VERSION,
};

/// `BLINKER_UPDATE_API`, honoured only by debug builds.
pub fn api_override() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        std::env::var(super::UPDATE_API_ENV).ok().filter(|s| !s.is_empty())
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

pub(super) fn local_http_ok() -> bool {
    api_override().is_some()
}

/// GET the release JSON (`_http_json(UPDATE_API)`), refusing a non-https URL.
pub fn fetch_release(url: &str, local_http_ok: bool) -> Result<Value, HttpError> {
    if !url_allowed(url, local_http_ok) {
        return Err(HttpError::Other(insecure_url(url)));
    }
    winhttp::get_json(url, JSON_TIMEOUT_MS)
}

/// The offer of the last check, applied by `apply` (the webview never chooses what gets
/// downloaded or installed). Every check replaces it.
static OFFER: Mutex<Option<Offer>> = Mutex::new(None);

pub(super) fn store_offer(c: &UpdateCheck) {
    let offer = match c {
        UpdateCheck::Available { offer, .. } => Some(offer.clone()),
        _ => None,
    };
    *OFFER.lock().unwrap_or_else(|e| e.into_inner()) = offer;
}

pub(super) fn take_offer() -> Option<Offer> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_refuses_plain_http_release_url() {
        let c = check_url("http://example.com/latest", false, "0.1.0");
        let UpdateCheck::Error { dialog: dg } = c else { panic!("{c:?}") };
        assert_eq!(dg.text, "Could not check for updates:\nrefusing a non-https URL: http://example.com/latest");
    }
}
