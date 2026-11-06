//! Startup guards: the WebView2 runtime check, a native error box for failures before
//! (or instead of) the webview, and the decision to show a window the frontend never
//! showed.

/// Microsoft's Evergreen Bootstrapper download link.
pub const WEBVIEW2_DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// The WebView2 Runtime's EdgeUpdate client id; its `pv` value is the installed version.
const WEBVIEW2_CLIENT: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    LocalMachine,
    CurrentUser,
}

/// Where Microsoft documents the runtime registers (per-machine on 64-bit and 32-bit
/// Windows, per-user).
fn webview2_keys() -> [(Hive, String); 3] {
    [
        (Hive::LocalMachine, format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2_CLIENT}")),
        (Hive::LocalMachine, format!(r"SOFTWARE\{WEBVIEW2_CLIENT}")),
        (Hive::CurrentUser, format!(r"Software\{WEBVIEW2_CLIENT}")),
    ]
}

/// Installed when any of the keys has a `pv` that is non-empty and not `0.0.0.0`
/// (Microsoft's documented detection; an uninstalled runtime can leave `0.0.0.0`).
pub fn webview2_installed(read_pv: impl Fn(Hive, &str) -> Option<String>) -> bool {
    webview2_keys().iter().any(|(hive, key)| {
        read_pv(*hive, key).is_some_and(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0.0.0.0"
        })
    })
}

/// The registry reader for `webview2_installed`. Debug builds treat the runtime as
/// missing when `BLINKER_SIMULATE_NO_WEBVIEW2` is set (manual check of the message).
#[cfg(windows)]
pub fn read_pv_registry(hive: Hive, key: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    #[cfg(debug_assertions)]
    if std::env::var_os("BLINKER_SIMULATE_NO_WEBVIEW2").is_some() {
        return None;
    }
    let root = match hive {
        Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        Hive::CurrentUser => HKEY_CURRENT_USER,
    };
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = "pv\0".encode_utf16().collect();
    let mut buf = [0u16; 128];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: valid NUL-terminated strings; `buf`/`size` describe a writable buffer.
    let rc = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc != 0 {
        return None;
    }
    let len = (size as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0').to_string())
}

#[cfg(not(windows))]
pub fn read_pv_registry(_hive: Hive, _key: &str) -> Option<String> {
    Some("n/a".into())
}

/// Text of the box shown when the runtime is missing.
pub fn missing_webview2_text() -> String {
    format!(
        "Blinker UI needs the Microsoft Edge WebView2 Runtime, which is not installed on this PC.\n\n\
         Install the Evergreen Runtime from:\n{WEBVIEW2_DOWNLOAD_URL}\n\n\
         Open the download page now?"
    )
}

/// Text of the box shown when the app could not start for any other reason.
pub fn startup_error_text(err: &str) -> String {
    format!(
        "Blinker UI could not start:\n\n{err}\n\n\
         If the Microsoft Edge WebView2 Runtime is missing or damaged, install it from:\n{WEBVIEW2_DOWNLOAD_URL}"
    )
}

/// Native error box (title `Blinker UI`). With `offer_download` it asks Yes/No and
/// opens the download page on Yes.
#[cfg(windows)]
pub fn error_box(text: &str, offer_download: bool) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_YESNO};
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (w(text), w("Blinker UI"));
    let buttons = if offer_download { MB_YESNO } else { MB_OK };
    // SAFETY: valid NUL-terminated strings, no owner window.
    let r = unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), buttons | MB_ICONERROR | MB_SETFOREGROUND) };
    if offer_download && r == IDYES {
        let (open, url) = (w("open"), w(WEBVIEW2_DOWNLOAD_URL));
        // SAFETY: valid NUL-terminated strings.
        unsafe { ShellExecuteW(std::ptr::null_mut(), open.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
    }
}

#[cfg(not(windows))]
pub fn error_box(text: &str, _offer_download: bool) {
    eprintln!("{text}");
}

/// How long the frontend gets to show the window itself before the backend shows it.
pub const FALLBACK_SHOW_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// The frontend never reported ready and the window is still hidden: it failed to load
/// or hung before `showWindow`, so show it (the user sees the state, a second launch
/// works).
pub fn fallback_should_show(reported_ready: bool, visible: bool) -> bool {
    !reported_ready && !visible
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webview2_detection() {
        assert!(webview2_installed(|h, k| (h == Hive::LocalMachine && k.contains("WOW6432Node")).then(|| "131.0.2903.70".into())));
        assert!(webview2_installed(|h, _| (h == Hive::CurrentUser).then(|| "131.0.2903.70".into())));
        assert!(!webview2_installed(|_, _| None), "absent");
        assert!(!webview2_installed(|_, _| Some("0.0.0.0".into())), "uninstalled leaves 0.0.0.0");
        assert!(!webview2_installed(|_, _| Some("  ".into())), "empty");
        assert!(webview2_installed(|h, k| match (h, k.contains("WOW6432Node")) {
            (Hive::LocalMachine, true) => Some("0.0.0.0".into()),
            (Hive::CurrentUser, _) => Some("140.0.1.2".into()),
            _ => None,
        }));
    }

    #[test]
    fn keys_are_the_documented_ones() {
        let k = webview2_keys();
        assert_eq!(k[0].1, r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}");
        assert_eq!(k[2], (Hive::CurrentUser, r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn this_machine_has_the_runtime() {
        // The dev machine and the GitHub windows runner both have WebView2.
        assert!(webview2_installed(read_pv_registry));
    }

    #[test]
    fn messages_carry_the_download_link() {
        assert!(missing_webview2_text().contains(WEBVIEW2_DOWNLOAD_URL));
        let t = startup_error_text("boom");
        assert!(t.contains("boom") && t.contains(WEBVIEW2_DOWNLOAD_URL));
    }

    #[test]
    fn fallback_shows_only_a_never_shown_window() {
        assert!(fallback_should_show(false, false));
        assert!(!fallback_should_show(true, false));
        assert!(!fallback_should_show(false, true));
        assert!(!fallback_should_show(true, true));
    }
}
