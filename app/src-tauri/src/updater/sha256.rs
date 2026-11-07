use std::path::Path;

use crate::pyerr;

#[cfg(test)]
use super::expected_hash_from_text;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::test_support::file_with;

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
}
