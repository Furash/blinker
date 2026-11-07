use std::io;
use std::path::{Path, PathBuf};

/// The repo-root `bootstrap.py`, copied into OUT_DIR by build.rs.
const BOOTSTRAP_PY: &str = include_str!(concat!(env!("OUT_DIR"), "/bootstrap.py"));
const BOOTSTRAP_NAME: &str = "blinker_bootstrap.py";

/// Writes the embedded bootstrap to `%TEMP%\blinker_bootstrap.py` (only if it differs).
pub fn write_bootstrap() -> io::Result<PathBuf> {
    write_bootstrap_to(&std::env::temp_dir())
}

/// Where `write_bootstrap` puts the script (for error messages).
pub fn bootstrap_path() -> PathBuf {
    std::env::temp_dir().join(BOOTSTRAP_NAME)
}

fn write_bootstrap_to(dir: &Path) -> io::Result<PathBuf> {
    let path = dir.join(BOOTSTRAP_NAME);
    if std::fs::read(&path).ok().as_deref() != Some(BOOTSTRAP_PY.as_bytes()) {
        std::fs::write(&path, BOOTSTRAP_PY)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bootstrap_written_matches_embedded() {
        let repo_copy = fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bootstrap.py")).unwrap();
        assert_eq!(BOOTSTRAP_PY.as_bytes(), &repo_copy[..], "embedded copy is the repo-root bootstrap.py");

        let t = tempfile::tempdir().unwrap();
        let p = write_bootstrap_to(t.path()).unwrap();
        assert_eq!(p, t.path().join("blinker_bootstrap.py"));
        assert_eq!(fs::read(&p).unwrap(), BOOTSTRAP_PY.as_bytes());

        // identical content is not rewritten (a read-only file would fail a write)
        let mut perm = fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&p, perm.clone()).unwrap();
        let again = write_bootstrap_to(t.path());
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(&p, perm).unwrap();
        assert_eq!(again.unwrap(), p);

        // stale content is replaced
        fs::write(&p, b"old").unwrap();
        write_bootstrap_to(t.path()).unwrap();
        assert_eq!(fs::read(&p).unwrap(), BOOTSTRAP_PY.as_bytes());
    }
}
