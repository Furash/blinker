//! Installed font families for the Settings dialog's font combo boxes
//! (`QFontDatabase.families()`). A webview cannot list system fonts without a
//! permission prompt (Local Font Access API), so the backend enumerates them with GDI.

/// Qt's list shape: no vertical (`@`) faces, no duplicates (GDI reports a family once
/// per charset), sorted case-insensitively.
pub fn clean_families(names: Vec<String>) -> Vec<String> {
    let mut v: Vec<String> = names.into_iter().filter(|n| !n.is_empty() && !n.starts_with('@')).collect();
    v.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
    v.dedup();
    v
}

/// LOGFONT face names stop at 31 characters ("Segoe UI Variable Display Semib"); the
/// full name (64 characters) then carries the whole family name when it extends it.
pub fn untruncated(face: String, full: String) -> String {
    if face.chars().count() >= 31 && full.starts_with(&face) && full.len() > face.len() {
        full
    } else {
        face
    }
}

#[cfg(windows)]
pub fn families() -> Vec<String> {
    use windows_sys::Win32::Foundation::LPARAM;
    use windows_sys::Win32::Graphics::Gdi::{
        EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, ENUMLOGFONTEXW, LOGFONTW, TEXTMETRICW,
    };

    fn wstr(s: &[u16]) -> String {
        String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
    }

    unsafe extern "system" fn cb(lf: *const LOGFONTW, _tm: *const TEXTMETRICW, _ty: u32, out: LPARAM) -> i32 {
        // SAFETY: EnumFontFamiliesExW passes an ENUMLOGFONTEXW (which starts with the
        // LOGFONTW); `out` is the Vec below, alive for the call.
        let (elf, out) = unsafe { (&*(lf as *const ENUMLOGFONTEXW), &mut *(out as *mut Vec<String>)) };
        out.push(untruncated(wstr(&elf.elfLogFont.lfFaceName), wstr(&elf.elfFullName)));
        1
    }

    let mut names: Vec<String> = Vec::new();
    // SAFETY: plain GDI calls on the screen DC, released before returning.
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return Vec::new();
        }
        let lf = LOGFONTW { lfCharSet: DEFAULT_CHARSET, ..Default::default() };
        EnumFontFamiliesExW(hdc, &lf, Some(cb), &mut names as *mut Vec<String> as LPARAM, 0);
        ReleaseDC(std::ptr::null_mut(), hdc);
    }
    clean_families(names)
}

#[cfg(not(windows))]
pub fn families() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_drops_vertical_and_duplicates_and_sorts_case_insensitively() {
        let v = ["Segoe UI", "@MS Gothic", "Arial", "AR PL UKai", "Segoe UI", "", "amiri", "8514oem"];
        assert_eq!(
            clean_families(v.iter().map(|s| s.to_string()).collect()),
            ["8514oem", "amiri", "AR PL UKai", "Arial", "Segoe UI"]
        );
    }

    #[test]
    fn untruncated_uses_full_name_only_for_cut_face_names() {
        let cut = "Segoe UI Variable Display Semib".to_string();
        assert_eq!(cut.len(), 31);
        assert_eq!(untruncated(cut.clone(), "Segoe UI Variable Display Semibold".into()), "Segoe UI Variable Display Semibold");
        assert_eq!(untruncated(cut.clone(), "Other".into()), cut);
        assert_eq!(untruncated("Arial".into(), "Arial Bold".into()), "Arial");
    }

    #[cfg(windows)]
    #[test]
    fn system_has_segoe_ui() {
        let f = families();
        assert!(f.iter().any(|n| n == "Segoe UI"), "{} families", f.len());
        assert!(f.iter().all(|n| !n.starts_with('@')));
    }
}
