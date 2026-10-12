//! Automatic boot assessment: blessing the counted entry a boot launched.
//!
//! Surfacer (the UEFI boot manager) counts a BLS type-1 entry by renaming its
//! file to `<id>+<left>[-<done>].conf` right before launching it. For a counted
//! Android launch it passes that file's ESP-root path to the kernel through the
//! bootconfig key `androidboot.surfacer.boot_count_path`, which Android exposes
//! as the property `ro.boot.surfacer.boot_count_path`; the same path also goes
//! to EFI-stage readers as the volatile Boot Loader Interface variable
//! `LoaderBootCountPath`. Direct-mode Android has no efivars, so the daemon
//! blesses from the property. Once the boot is judged successful, blessing
//! renames the file to its uncounted `<id>.conf` name, so its tries start over;
//! without a bless every counted entry eventually exhausts and Surfacer stops
//! selecting it automatically.
//!
//! Blessing only ever renames the one file the path names, durably. An absent
//! (empty) path, a path that names no counted entry, and a file another
//! bootstrap already blessed are all no-ops, so blessing twice is harmless.
use crate::{context::ESP, fsutil as fsu};
use anyhow::{Result, bail, ensure};
use std::fs;
use std::path::{Path, PathBuf};

/// The counted entry file a boot count path names and the uncounted name
/// blessing renames it to; both are relative to the ESP root.
#[derive(Debug, PartialEq, Eq)]
pub struct Blessing {
    pub counted: PathBuf,
    pub blessed: PathBuf,
}

/// The ESP-relative file a boot count path names.
///
/// The bootconfig value is absolute from the ESP root with `/` separators
/// (`/loader/entries/<id>+<left>[-<done>].conf`); the EFI-stage form with `\`
/// separators is accepted too. `None` for anything that is not one file inside
/// the ESP: an empty value, a `:` drive/stream separator, a control character,
/// or an empty, `.` or `..` component. A path that is merely not a counted entry
/// is rejected later by [`blessed_name`].
fn esp_file(value: &str) -> Option<PathBuf> {
    let relative = value.trim_start_matches(['\\', '/']);
    let parts = relative.split(['\\', '/']).collect::<Vec<_>>();
    let inside = !relative.is_empty()
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && !part
                    .bytes()
                    .any(|byte| byte < 0x20 || byte == 0x7f || byte == b':')
        });
    inside.then(|| parts.into_iter().collect())
}

/// The name blessing renames a counted entry file to: `<id>.conf`.
///
/// The counter grammar matches the loader's exactly (`+<left>` or `+<left>-<done>`
/// with decimal fields), so a name the loader does not count is never rewritten.
/// `None` is a no-op: no counter at all, which covers an already blessed
/// `<id>.conf` and any name outside the boot assessment scheme.
pub fn blessed_name(file_name: &str) -> Option<String> {
    let split = file_name.len().checked_sub(".conf".len())?;
    if !file_name.is_char_boundary(split) {
        return None;
    }
    let (stem, suffix) = file_name.split_at(split);
    if !suffix.eq_ignore_ascii_case(".conf") {
        return None;
    }
    let plus = stem.rfind('+')?;
    let counter = &stem[plus + 1..];
    let (left, done) = match counter.split_once('-') {
        Some(fields) => fields,
        None => (counter, ""),
    };
    if !decimal(left) || (counter.contains('-') && !decimal(done)) {
        return None;
    }
    Some(format!("{}{suffix}", &stem[..plus]))
}

/// A decimal counter field: non-empty ASCII digits only.
fn decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Decide the blessing of one boot count path, without touching a filesystem.
///
/// `Ok(None)` is the no-op case: an absent (empty) path, or one that names no
/// counted entry to bless. An `Err` is a path that cannot be one file inside the
/// ESP at all, which is corruption rather than a legitimately uncounted launch.
pub fn blessing(value: &str) -> Result<Option<Blessing>> {
    if value.is_empty() {
        return Ok(None);
    }
    let Some(counted) = esp_file(value) else {
        bail!("boot count path is not an ESP path: {value:?}");
    };
    let Some(name) = counted
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(blessed_name)
    else {
        return Ok(None);
    };
    Ok(Some(Blessing {
        blessed: counted.with_file_name(name),
        counted,
    }))
}

/// Bless the counted entry a boot launched from its boot count path, and return
/// the blessed path.
///
/// An absent (empty) path, a path naming no counted entry, and an already
/// blessed (gone) file are no-ops; nothing but the one named file is ever
/// renamed. The rename is durable: both directories are fsynced.
pub fn bless_counted_entry(value: &str) -> Result<Option<PathBuf>> {
    let Some(Blessing { counted, blessed }) = blessing(value)? else {
        return Ok(None);
    };
    ensure!(
        fsu::mounted(Path::new(ESP))?,
        "ESP is not mounted at {ESP}; cannot bless {}",
        counted.display()
    );
    let source = Path::new(ESP).join(&counted);
    match fs::symlink_metadata(&source) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        metadata => ensure!(
            metadata?.is_file(),
            "{} is not a boot entry file",
            counted.display()
        ),
    }
    let target = Path::new(ESP).join(&blessed);
    fsu::rename(&source, &target)?;
    Ok(Some(target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative(parts: &[&str]) -> PathBuf {
        parts.iter().collect()
    }

    /// The whole pure decision: map the boot count path to the counted file and
    /// its blessed name, and no-op wherever nothing is blessable.
    #[test]
    fn boot_count_path_decides_the_blessed_entry() {
        let counted = |name: &str, blessed: &str| {
            Some(Blessing {
                counted: relative(&["loader", "entries", name]),
                blessed: relative(&["loader", "entries", blessed]),
            })
        };
        // A counted launch blesses that one entry, in its own directory, with the
        // counter dropped from the name and the rest of the name kept as written.
        assert_eq!(
            blessing("/loader/entries/android-direct+2-1.conf").unwrap(),
            counted("android-direct+2-1.conf", "android-direct.conf")
        );
        assert_eq!(
            blessing("/loader/entries/Android-Direct+3.conf").unwrap(),
            counted("Android-Direct+3.conf", "Android-Direct.conf")
        );
        assert_eq!(
            blessing("/loader/entries/rom1/android-direct+0-4.conf").unwrap(),
            Some(Blessing {
                counted: relative(&["loader", "entries", "rom1", "android-direct+0-4.conf"]),
                blessed: relative(&["loader", "entries", "rom1", "android-direct.conf"]),
            })
        );
        // The EFI-stage form of the same path is accepted too.
        assert_eq!(
            blessing(r"\loader\entries\android-direct+1-1.conf").unwrap(),
            counted("android-direct+1-1.conf", "android-direct.conf")
        );
        // Nothing to bless: no counted launch this boot (an absent bootconfig key
        // arrives empty), and names outside the boot assessment scheme,
        // including an already blessed entry.
        assert_eq!(blessing("").unwrap(), None);
        for none in [
            "/loader/entries/android-direct.conf",
            "/loader/entries/Android-Direct.conf",
            "/loader/entries/android-direct+x.conf",
            "/loader/entries/android-direct+2-.conf",
            "/loader/entries/android-direct+2-1.name.conf",
            "/loader/entries/android-direct+2-1.txt",
            "/loader/entries",
        ] {
            assert_eq!(blessing(none).unwrap(), None, "{none}");
        }
        // A path that is not one file inside the ESP is corruption, not a bless.
        for escaped in [
            "/../../loader/entries/android-direct+2-1.conf",
            "/loader//entries/android-direct+2-1.conf",
            "/loader/entries/../../android-direct+2-1.conf",
            "/loader/entries/./android-direct+2-1.conf",
            "/",
            "C:/loader/entries/android-direct+2-1.conf",
            "/loader/entries/\u{7}android-direct+2-1.conf",
        ] {
            assert!(blessing(escaped).is_err(), "{escaped}");
        }
    }
}
