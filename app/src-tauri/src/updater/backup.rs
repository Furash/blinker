//! Pre-update backup: before the updater script is started, the user's settings are
//! copied to `<config dir>\backups\pre-update-<version>-<YYYYMMDD-HHMMSS>\` (UTC), so an
//! update can never lose them, whatever happens to the install folder. The newest
//! `KEEP_BACKUPS` such folders are kept.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Folder under the config dir that holds the backups.
pub const BACKUPS_DIR: &str = "backups";
/// How many `pre-update-*` folders are kept.
pub const KEEP_BACKUPS: usize = 5;
const PREFIX: &str = "pre-update-";
const CONFIG_FILE: &str = "blinker_ui_config.json";

/// `YYYYMMDD-HHMMSS` of `now` in UTC (no time-zone database needed). Times before 1970
/// are clamped to the epoch.
pub fn utc_stamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days (days since 1970-01-01 -> y/m/d), unsigned form.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

/// The version as a safe path component: ASCII letters, digits, `.`, `-`, `_` are kept,
/// anything else becomes `_`; leading/trailing dots and underscores are trimmed; an empty
/// result is `unknown`.
pub fn sanitize_version(v: &str) -> String {
    let s: String = v
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let s = s.trim_matches(|c| c == '.' || c == '_');
    if s.is_empty() { "unknown".into() } else { s.into() }
}

/// `(stamp, counter)` of a `pre-update-<version>-<YYYYMMDD-HHMMSS>[-<n>]` name, else None.
fn backup_key(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix(PREFIX)?;
    let parts: Vec<&str> = rest.split('-').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let n = parts.len();
    let (date, time, counter, version_parts) = match parts.as_slice() {
        [.., d, t] if n >= 3 && digits(d, 8) && digits(t, 6) => (*d, *t, 1, n - 2),
        [.., d, t, c] if n >= 4 && digits(d, 8) && digits(t, 6) && (1..=4).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_digit()) => {
            (*d, *t, c.parse().ok()?, n - 3)
        }
        _ => return None,
    };
    (version_parts > 0 && !parts[..version_parts].join("-").is_empty()).then(|| (format!("{date}-{time}"), counter))
}

/// `Ok(None)` for a missing item (skipped), the metadata of an existing one, and an error
/// for anything else (it exists but cannot be examined: the backup must not skip it).
fn existing(src: &Path, r: std::io::Result<std::fs::Metadata>) -> Result<Option<std::fs::Metadata>, String> {
    match r {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {}: {e}", src.display())),
    }
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::copy(src, dst).map(drop).map_err(|e| format!("could not copy {} to {}: {e}", src.display(), dst.display()))
}

/// Recursive copy. Symlinks and junctions are skipped, never followed (a junction loop
/// must not recurse forever before the updater is started).
fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir(dst).map_err(|e| format!("could not create {}: {e}", dst.display()))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("could not read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", src.display()))?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let Some(meta) = existing(&from, std::fs::symlink_metadata(&from))? else { continue };
        if meta.file_type().is_symlink() {
            continue;
        } else if meta.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            copy_file(&from, &to)?;
        }
    }
    Ok(())
}

/// Create `<backups>\pre-update-<v>-<stamp>` (with `-2`, `-3`, ... when it exists).
fn create_backup_dir(backups: &Path, base: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(backups).map_err(|e| format!("could not create {}: {e}", backups.display()))?;
    for n in 1..=100u32 {
        let dir = backups.join(if n == 1 { base.to_string() } else { format!("{base}-{n}") });
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create {}: {e}", dir.display())),
        }
    }
    Err(format!("could not create a new backup folder in {}", backups.display()))
}

/// Delete all but the `KEEP_BACKUPS` newest `pre-update-*` folders (by the time in the
/// name); `keep` (the new backup) always stays. Other names are never touched; errors
/// are ignored (an old backup that cannot be deleted does no harm).
fn prune(backups: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(backups) else { return };
    let mut old: Vec<((String, u32), PathBuf)> = entries
        .flatten()
        .filter(|e| e.path() != keep && e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| backup_key(&e.file_name().to_string_lossy()).map(|k| (k, e.path())))
        .collect();
    old.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, dir) in old.into_iter().skip(KEEP_BACKUPS.saturating_sub(1)) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Copy the user's settings to a new `pre-update-*` folder (see the module doc) and prune
/// older ones. Copied when they exist: `blinker_ui_config.json` and `.json.bak` and the
/// `icons\` folder (as `config-icons\`) of `config_dir`; the legacy
/// `blinker_ui_config.json` (as `install-blinker_ui_config.json`) and the `icons\` folder
/// (as `install-icons\`) of `install_dir`. Missing items are skipped, and so are
/// symlinks/junctions (an `icons` link is not followed); an item that exists but cannot
/// be examined or copied, or a failure to create the folder, is an error, and the
/// incomplete folder is removed.
pub fn backup_before_update(install_dir: &Path, config_dir: &Path, from_version: &str, now: SystemTime) -> Result<PathBuf, String> {
    let backups = config_dir.join(BACKUPS_DIR);
    let dir = create_backup_dir(&backups, &format!("{PREFIX}{}-{}", sanitize_version(from_version), utc_stamp(now)))?;
    let items: [(PathBuf, &str, bool); 5] = [
        (config_dir.join(CONFIG_FILE), CONFIG_FILE, false),
        (config_dir.join(format!("{CONFIG_FILE}.bak")), "blinker_ui_config.json.bak", false),
        (config_dir.join("icons"), "config-icons", true),
        (install_dir.join(CONFIG_FILE), "install-blinker_ui_config.json", false),
        (install_dir.join("icons"), "install-icons", true),
    ];
    let copy_all = || -> Result<(), String> {
        for (src, name, is_dir) in &items {
            let Some(meta) = existing(src, std::fs::symlink_metadata(src))? else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            match (is_dir, meta.is_dir()) {
                (true, true) => copy_dir(src, &dir.join(name))?,
                (true, false) => {} // a file named `icons`: not a folder of icons
                (false, _) => copy_file(src, &dir.join(name))?,
            }
        }
        Ok(())
    };
    if let Err(e) = copy_all() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    prune(&backups, &dir);
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn utc_stamp_formats_fixed_times() {
        assert_eq!(utc_stamp(at(0)), "19700101-000000");
        // 2026-11-07 13:05:09 UTC
        assert_eq!(utc_stamp(at(1_794_056_709)), "20261107-130509");
        // leap day 2024-02-29 23:59:59 UTC
        assert_eq!(utc_stamp(at(1_709_251_199)), "20240229-235959");
        // 2000-03-01 00:00:00 UTC
        assert_eq!(utc_stamp(at(951_868_800)), "20000301-000000");
        // before the epoch is clamped to it
        assert_eq!(utc_stamp(UNIX_EPOCH - Duration::from_secs(5)), "19700101-000000");
    }

    #[test]
    fn version_is_sanitised_for_a_path() {
        assert_eq!(sanitize_version("0.2.1"), "0.2.1");
        assert_eq!(sanitize_version("0.3.0-rc.1"), "0.3.0-rc.1");
        assert_eq!(sanitize_version("..\\..\\evil/x:y*"), "evil_x_y");
        assert_eq!(sanitize_version("1.0 beta"), "1.0_beta");
        assert_eq!(sanitize_version(""), "unknown");
        assert_eq!(sanitize_version("..."), "unknown");
        assert_eq!(sanitize_version("é"), "unknown");
    }

    #[test]
    fn backup_copies_config_icons_and_legacy_install_items() {
        let tmp = tempfile::tempdir().unwrap();
        let (inst, cfg) = (tmp.path().join("BlinkerUI"), tmp.path().join("cfg"));
        write(&cfg.join("blinker_ui_config.json"), "cfg");
        write(&cfg.join("blinker_ui_config.json.bak"), "bak");
        write(&cfg.join("icons").join("mine.png"), "user icon");
        write(&cfg.join("icons").join("sub").join("deep.svg"), "deep");
        write(&cfg.join("other.txt"), "not backed up");
        write(&inst.join("blinker_ui_config.json"), "legacy");
        write(&inst.join("icons").join("claude.svg"), "bundled or custom");
        write(&inst.join("BlinkerUI.exe"), "exe is not backed up");
        let b = backup_before_update(&inst, &cfg, "0.2.1", at(1_794_056_709)).unwrap();
        assert_eq!(b, cfg.join("backups").join("pre-update-0.2.1-20261107-130509"));
        assert_eq!(
            names(&b),
            [
                "blinker_ui_config.json",
                "blinker_ui_config.json.bak",
                "config-icons",
                "install-blinker_ui_config.json",
                "install-icons"
            ]
        );
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(b.join("blinker_ui_config.json")), "cfg");
        assert_eq!(read(b.join("blinker_ui_config.json.bak")), "bak");
        assert_eq!(read(b.join("config-icons").join("mine.png")), "user icon");
        assert_eq!(read(b.join("config-icons").join("sub").join("deep.svg")), "deep");
        assert_eq!(read(b.join("install-blinker_ui_config.json")), "legacy");
        assert_eq!(read(b.join("install-icons").join("claude.svg")), "bundled or custom");
        // the sources are untouched
        assert_eq!(read(cfg.join("blinker_ui_config.json")), "cfg");
        assert!(inst.join("icons").join("claude.svg").exists());
    }

    #[test]
    fn missing_optional_items_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let (inst, cfg) = (tmp.path().join("inst"), tmp.path().join("cfg"));
        // nothing exists at all (not even the folders): an empty backup folder
        let b = backup_before_update(&inst, &cfg, "0.2.1", at(0)).unwrap();
        assert!(b.is_dir());
        assert!(names(&b).is_empty());
        // only the config
        write(&cfg.join("blinker_ui_config.json"), "{}");
        let b = backup_before_update(&inst, &cfg, "0.2.1", at(1)).unwrap();
        assert_eq!(names(&b), ["blinker_ui_config.json"]);
    }

    #[test]
    fn same_second_twice_gets_a_new_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        let a = backup_before_update(tmp.path(), &cfg, "0.2.1", at(0)).unwrap();
        let b = backup_before_update(tmp.path(), &cfg, "0.2.1", at(0)).unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        assert_eq!(b.file_name().unwrap(), "pre-update-0.2.1-19700101-000000-2");
    }

    #[test]
    fn copy_failure_of_an_existing_item_aborts_and_removes_the_partial_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        write(&cfg.join("blinker_ui_config.json.bak"), "bak");
        // the config path exists but cannot be copied as a file
        std::fs::create_dir_all(cfg.join("blinker_ui_config.json")).unwrap();
        let e = backup_before_update(tmp.path(), &cfg, "0.2.1", at(0)).unwrap_err();
        assert!(e.contains("blinker_ui_config.json"), "{e}");
        assert!(names(&cfg.join("backups")).is_empty(), "partial backup removed");
    }

    #[test]
    fn only_missing_items_are_skipped() {
        let p = Path::new("C:\\cfg\\blinker_ui_config.json");
        let nf = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(existing(p, Err(nf)).map(|m| m.is_some()), Ok(false));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let e = existing(p, Err(denied)).unwrap_err();
        assert!(e.starts_with("could not read C:\\cfg\\blinker_ui_config.json: "), "{e}");
    }

    #[cfg(windows)]
    #[test]
    fn an_item_that_cannot_be_examined_aborts() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        write(&cfg.join("blinker_ui_config.json"), "{}");
        // metadata fails with ERROR_INVALID_NAME, not "not found"
        let e = backup_before_update(&tmp.path().join("in<valid"), &cfg, "0.2.1", at(0)).unwrap_err();
        assert!(e.contains("in<valid"), "{e}");
        assert!(names(&cfg.join("backups")).is_empty(), "partial backup removed");
    }

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "mklink /J {}", link.display());
    }

    #[cfg(windows)]
    #[test]
    fn links_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let (inst, cfg) = (tmp.path().join("inst"), tmp.path().join("cfg"));
        write(&cfg.join("icons").join("mine.png"), "user icon");
        junction(&cfg.join("icons").join("loop"), &cfg.join("icons"));
        // the install's icons folder itself is a junction
        write(&tmp.path().join("elsewhere").join("x.png"), "x");
        std::fs::create_dir_all(&inst).unwrap();
        junction(&inst.join("icons"), &tmp.path().join("elsewhere"));
        let b = backup_before_update(&inst, &cfg, "0.2.1", at(0)).unwrap();
        assert_eq!(names(&b), ["config-icons"]);
        assert_eq!(names(&b.join("config-icons")), ["mine.png"]);
    }

    #[test]
    fn unwritable_backups_folder_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        write(&cfg.join("blinker_ui_config.json"), "{}");
        write(&cfg.join("backups"), "a file where the folder should be");
        let e = backup_before_update(tmp.path(), &cfg, "0.2.1", at(0)).unwrap_err();
        assert!(e.contains("backups"), "{e}");
        assert_eq!(std::fs::read_to_string(cfg.join("backups")).unwrap(), "a file where the folder should be");
    }

    #[test]
    fn pruning_keeps_the_five_newest_and_ignores_foreign_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        let backups = cfg.join("backups");
        // older backups of other versions: newest by time, not by name
        for (v, stamp) in [
            ("0.9.0", "20200101-000000"),
            ("0.1.0", "20250101-000000"),
            ("0.2.0", "20250102-000000"),
            ("0.2.0", "20250102-000000-2"),
            ("0.0.1", "20260101-000000"),
            ("0.2.1", "20260102-000000"),
        ] {
            write(&backups.join(format!("pre-update-{v}-{stamp}")).join("blinker_ui_config.json"), "old");
        }
        std::fs::create_dir_all(backups.join("my-own-copy")).unwrap();
        std::fs::create_dir_all(backups.join("pre-update-garbage")).unwrap();
        write(&backups.join("pre-update-0.1.0-20000101-000000.txt"), "a file, not a backup folder");
        let b = backup_before_update(tmp.path(), &cfg, "0.2.1", at(1_794_056_709)).unwrap();
        assert_eq!(
            names(&backups),
            [
                "my-own-copy",
                "pre-update-0.0.1-20260101-000000",
                "pre-update-0.1.0-20000101-000000.txt",
                "pre-update-0.2.0-20250102-000000",
                "pre-update-0.2.0-20250102-000000-2",
                "pre-update-0.2.1-20260102-000000",
                "pre-update-0.2.1-20261107-130509",
                "pre-update-garbage",
            ]
        );
        assert!(b.is_dir());
    }

    #[test]
    fn the_new_backup_survives_pruning_even_with_a_clock_in_the_past() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        for d in 1..=5 {
            std::fs::create_dir_all(cfg.join("backups").join(format!("pre-update-0.2.0-2030010{d}-000000"))).unwrap();
        }
        let b = backup_before_update(tmp.path(), &cfg, "0.2.1", at(0)).unwrap();
        assert!(b.is_dir());
        assert_eq!(names(&cfg.join("backups")).len(), 5);
        assert!(!cfg.join("backups").join("pre-update-0.2.0-20300101-000000").exists());
    }
}
