//! Automatic boot assessment: blessing the counted entry a boot launched.
//!
//! Surfacer (the UEFI boot manager) counts a BLS type-1 entry by renaming its
//! file to `<id>+<left>[-<done>].conf` right before launching it and publishes
//! the renamed ESP path in the volatile Boot Loader Interface variable
//! `LoaderBootCountPath`. Once that boot is judged successful, userspace blesses
//! the attempt by renaming the file to its uncounted `<id>.conf` name, so its
//! tries start over. Without a bless, every counted entry eventually exhausts
//! and Surfacer stops selecting it automatically.
//!
//! Blessing only ever renames the one file the variable names, durably. An
//! absent variable (no counted launch this boot, or no EFI variable store), a
//! value that names no counted entry, and a file that another bootstrap already
//! blessed are all no-ops, so blessing twice is harmless.
use crate::{context::ESP, fsutil as fsu};
use anyhow::{Context, Result, bail, ensure};
use std::fs;
use std::path::{Path, PathBuf};

/// Boot Loader Interface variable naming the counted entry file of this boot.
pub const BOOT_COUNT_VARIABLE: &str = "LoaderBootCountPath";
/// The Boot Loader Interface vendor GUID `4a67b082-0a4c-41cf-b6c7-440b29bb8c4f`.
const BOOT_LOADER_INTERFACE_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
/// Runtime EFI variable store, restored by [`crate::bootstrap::restore_efivarfs`].
const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// The counted entry file a boot count path names and the uncounted name
/// blessing renames it to; both are relative to the ESP root.
#[derive(Debug, PartialEq, Eq)]
pub struct Blessing {
    pub counted: PathBuf,
    pub blessed: PathBuf,
}

/// The efivarfs file holding this boot's counted entry path.
fn boot_count_efivar() -> PathBuf {
    Path::new(EFIVARS).join(format!(
        "{BOOT_COUNT_VARIABLE}-{BOOT_LOADER_INTERFACE_GUID}"
    ))
}

/// Text of an efivars value: a 4-byte attribute word, then NUL-terminated
/// UTF-16LE. The variable is volatile, so a value present during this boot names
/// a launch of this boot; its attribute bits are therefore not re-inspected.
///
/// `None` is an empty value, which names no file.
fn variable_text(bytes: &[u8]) -> Result<Option<String>> {
    let data = bytes
        .get(4..)
        .context("boot count value is shorter than its attribute word")?;
    if data.is_empty() {
        return Ok(None);
    }
    ensure!(data.len() % 2 == 0, "boot count value is not UTF-16 units");
    let units = data
        .chunks(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect::<Vec<_>>();
    let length = units
        .iter()
        .position(|unit| *unit == 0)
        .context("boot count value is not NUL-terminated")?;
    let text = String::from_utf16(&units[..length]).context("boot count value is not UTF-16")?;
    Ok((!text.is_empty()).then_some(text))
}

/// The ESP-relative file a boot count path names.
///
/// The loader's own form is absolute from the ESP root with `\` separators
/// (`\loader\entries\<id>+<left>[-<done>].conf`); `/` is accepted as a separator
/// too. `None` for anything that is not one file inside the ESP: an empty value,
/// a `:` drive/stream separator, a control character, or an empty, `.` or `..`
/// component. A path that is merely not a counted entry is rejected later by
/// [`blessed_name`].
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

/// Decide the blessing of one boot count value, without touching a filesystem.
///
/// `Ok(None)` is the no-op case: the value names no counted entry to bless. An
/// `Err` is a value that cannot be a path at all, which is corruption rather
/// than a legitimately uncounted launch.
pub fn blessing(bytes: &[u8]) -> Result<Option<Blessing>> {
    let Some(value) = variable_text(bytes)? else {
        return Ok(None);
    };
    let Some(counted) = esp_file(&value) else {
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

/// Bless the counted entry this boot launched, if any, and return its blessed
/// path.
///
/// A missing variable, a value naming no counted entry, and an already blessed
/// (gone) file are no-ops; nothing but the one named file is ever renamed.
pub fn bless_loaded_entry() -> Result<Option<PathBuf>> {
    let variable = boot_count_efivar();
    let bytes = match fs::read(&variable) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        bytes => bytes.with_context(|| format!("read {}", variable.display()))?,
    };
    let Some(Blessing { counted, blessed }) = blessing(&bytes)? else {
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

    /// A volatile Boot Loader Interface value: attribute word then NUL-terminated
    /// UTF-16LE text, exactly as efivarfs exposes it.
    fn value(text: &str) -> Vec<u8> {
        let mut bytes = vec![6, 0, 0, 0]; // BOOTSERVICE_ACCESS | RUNTIME_ACCESS
        bytes.extend(text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        bytes
    }

    fn relative(parts: &[&str]) -> PathBuf {
        parts.iter().collect()
    }

    /// The whole pure decision: decode the variable, map the counted entry name to
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
            blessing(&value(r"\loader\entries\android-direct+2-1.conf")).unwrap(),
            counted("android-direct+2-1.conf", "android-direct.conf")
        );
        assert_eq!(
            blessing(&value(r"\loader\entries\Android-Direct+3.conf")).unwrap(),
            counted("Android-Direct+3.conf", "Android-Direct.conf")
        );
        assert_eq!(
            blessing(&value(r"\loader\entries\rom1\android-direct+0-4.conf")).unwrap(),
            Some(Blessing {
                counted: relative(&["loader", "entries", "rom1", "android-direct+0-4.conf"]),
                blessed: relative(&["loader", "entries", "rom1", "android-direct.conf"]),
            })
        );
        // The value is ESP-absolute; an implied leading separator is accepted too.
        assert_eq!(
            blessing(&value("loader/entries/android-direct+1-1.conf")).unwrap(),
            counted("android-direct+1-1.conf", "android-direct.conf")
        );
        // Nothing to bless: no counted launch this boot, an empty value, and names
        // outside the boot assessment scheme (including an already blessed entry).
        for none in [
            r"\loader\entries\android-direct.conf",
            r"\loader\entries\Android-Direct.conf",
            r"\loader\entries\android-direct+x.conf",
            r"\loader\entries\android-direct+2-.conf",
            r"\loader\entries\android-direct+2-1.name.conf",
            r"\loader\entries\android-direct+2-1.txt",
            r"\loader\entries",
        ] {
            assert_eq!(blessing(&value(none)).unwrap(), None, "{none}");
        }
        assert_eq!(blessing(&value("")).unwrap(), None);
        // The attribute word is not text, and units after the terminator are not
        // path data: this is still exactly one counted entry.
        let mut padded = value(r"\loader\entries\android-direct+2-1.conf");
        padded.extend([b'j', 0, b'u', 0, b'n', 0, b'k', 0]);
        assert_eq!(
            blessing(&padded).unwrap(),
            counted("android-direct+2-1.conf", "android-direct.conf")
        );
        assert_eq!(blessing(&[6, 0, 0, 0]).unwrap(), None);
        // A value shorter than its attribute word, an odd byte count and a value
        // without a terminator are corruption, not an uncounted launch.
        for malformed in [
            vec![],
            vec![6, 0, 0],
            vec![6, 0, 0, 0, b'a'],
            vec![6, 0, 0, 0, b'a', 0],
        ] {
            assert!(blessing(&malformed).is_err(), "{malformed:?}");
        }
        // So is a value that is not one file inside the ESP.
        for escaped in [
            r"..\..\loader\entries\android-direct+2-1.conf",
            r"\loader\\entries\android-direct+2-1.conf",
            r"\loader\entries\..\..\android-direct+2-1.conf",
            r"C:\loader\entries\android-direct+2-1.conf",
            r"\\",
        ] {
            assert!(blessing(&value(escaped)).is_err(), "{escaped}");
        }
    }
}
