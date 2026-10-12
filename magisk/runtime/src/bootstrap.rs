use crate::context::*;
use crate::fsutil as fsu;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn stage_tools(config: &BootstrapConfig) -> Result<()> {
    let mut tools = BTreeSet::from(BOOTSTRAP_TOOLS);
    tools.insert(config.backing.helper.as_str());
    tools.extend(config.tools.iter().map(String::as_str));
    for name in tools {
        identifier(name)?;
        let independent = Path::new(INDEPENDENT_BIN).join(name);
        let source = match fs::symlink_metadata(&independent) {
            Ok(_) => independent,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Path::new(PACKAGE).join("bin").join(name)
            }
            Err(error) => return Err(error).context("stat bootstrap tool"),
        };
        fsu::regular(&source)?;
        let target = Path::new(BIN).join(name);
        ensure!(!target.exists(), "bootstrap tool collision {name}");
        fs::copy(&source, &target).with_context(|| format!("stage tool {}", source.display()))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
    }
    let resetprop = Path::new(BIN).join("resetprop");
    ensure!(!resetprop.exists(), "resetprop applet collision");
    std::os::unix::fs::symlink(DAEMON, resetprop)?;
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackingResult {
    owned_mounts: Vec<PathBuf>,
}
/// Helper CLI (all arguments literal, no shell):
/// `maos-credentials prepare-backing --kind block|filesystem
/// (--source ABS | --partition NAME) --fs-type TYPE --mount-at ABS
/// --store /dev/egysk/store [args...]`.
/// stdout: JSON `{ "owned_mounts": ["/actual/owned/mount", "/dev/egysk/store"] }`.
/// Paths are creation order, only newly owned mounts, never Android/vendor mounts.
/// The helper coordinates an existing Android metadata mount and never formats.
pub fn prepare_backing(config: &BootstrapConfig) -> Result<Vec<PathBuf>> {
    let backing = &config.backing;
    let output_path = Path::new(ROOT).join("backing-result");
    let output = fs::File::create(&output_path)?;
    let mut command = Command::new(Path::new(BIN).join(&backing.helper));
    crate::scripts::child_signals(&mut command);
    command.process_group(0).arg("prepare-backing").args([
        "--kind",
        &backing.kind,
        "--fs-type",
        &backing.fs_type,
        "--mount-at",
        &backing.mount_at,
        "--store",
        STORE,
    ]);
    if let Some(source) = &backing.source {
        ensure!(
            Path::new(source).exists(),
            "explicit backing source is unavailable: {source}"
        );
        command.args(["--source", source]);
    } else {
        command.args([
            "--partition",
            backing
                .partition
                .as_deref()
                .context("missing backing selector")?,
        ]);
    }
    let mut child = command
        .args(&backing.args)
        .stdin(Stdio::null())
        .stdout(output)
        .spawn()
        .context("start physical metadata bootstrap")?;
    crate::scripts::wait_until(&mut child, Instant::now() + Duration::from_secs(35), false)
        .with_context(|| {
            format!(
                "physical metadata bootstrap {} prepare-backing",
                backing.helper
            )
        })?;
    let result: BackingResult = serde_json::from_str(&fsu::text(&output_path, 4096)?)?;
    ensure!(
        fsu::mounted(Path::new(STORE))?,
        "helper did not expose metadata store bind"
    );
    let mut seen = BTreeSet::new();
    for path in &result.owned_mounts {
        absolute(path.to_str().context("non UTF-8 helper mount")?)?;
        ensure!(
            path == Path::new(STORE) || path == Path::new(&backing.mount_at),
            "helper returned unrelated mount {}",
            path.display()
        );
        ensure!(
            seen.insert(path) && fsu::mounted(path)?,
            "invalid helper mount ownership"
        );
    }
    Ok(result.owned_mounts)
}

pub fn initialize_store() -> Result<()> {
    for name in [
        "modules",
        "modules_update",
        "modules_previous",
        "state",
        "overlays",
        "preferences",
        "log",
        "post-fs-data.d",
        "service.d",
        "config",
    ] {
        fsu::directory(&Path::new(STORE).join(name))?;
    }
    fsu::directory(Path::new(MODULES))?;
    fsu::directory(&Path::new(ROOT).join("state"))
}
pub(crate) fn label(path: &Path, context: &str) -> Result<()> {
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    let context = std::ffi::CString::new(context)?;
    let mut current = [0u8; 256];
    // SAFETY: path/name are NUL-terminated and current is a writable buffer.
    let length = unsafe {
        libc::lgetxattr(
            name.as_ptr(),
            c"security.selinux".as_ptr(),
            current.as_mut_ptr().cast(),
            current.len(),
        )
    };
    if length >= 0 {
        let value = &current[..length as usize];
        if value.strip_suffix(&[0]).unwrap_or(value) == context.as_bytes() {
            return Ok(());
        }
    } else {
        let error = std::io::Error::last_os_error();
        ensure!(
            matches!(error.raw_os_error(), Some(libc::ENODATA | libc::ERANGE)),
            "read label {}: {error}",
            path.display()
        );
    }
    // SAFETY: pointers reference live NUL-terminated strings and context bytes.
    ensure!(
        unsafe {
            libc::lsetxattr(
                name.as_ptr(),
                c"security.selinux".as_ptr(),
                context.as_ptr().cast(),
                context.as_bytes_with_nul().len(),
                0,
            )
        } == 0,
        "label {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    Ok(())
}
pub(crate) fn label_tree(root: &Path, context: &str) -> Result<()> {
    label(root, context)?;
    if fs::symlink_metadata(root)?.is_dir() {
        for path in fsu::children(root)? {
            label_tree(&path, context)?;
        }
    }
    Ok(())
}
fn initialize_esp_permissions(root: &Path) -> Result<()> {
    fn permissions(root: &Path, path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let relative = path.strip_prefix(root)?;
        let executable = relative
            .components()
            .any(|part| matches!(part.as_os_str().to_str(), Some("bin" | "xbin")))
            || path.extension().and_then(|value| value.to_str()) == Some("sh");
        let mode = if metadata.is_dir() || executable {
            0o755
        } else {
            0o644
        };
        if metadata.permissions().mode() & 0o7777 != mode {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .with_context(|| format!("initialize module permissions: {}", path.display()))?;
        }
        if metadata.is_dir() {
            for child in fsu::children(path)? {
                permissions(root, &child)?;
            }
        }
        Ok(())
    }
    permissions(root, root)
}

fn label_module_tree(root: &Path) -> Result<()> {
    fn visit(root: &Path, path: &Path) -> Result<()> {
        let relative = path.strip_prefix(root)?;
        let context = if relative.starts_with("system/vendor") {
            "u:object_r:vendor_file:s0"
        } else if relative.starts_with("system") {
            "u:object_r:system_file:s0"
        } else {
            FILE_CONTEXT
        };
        label(path, context)?;
        if fs::symlink_metadata(path)?.is_dir() {
            for child in fsu::children(path)? {
                visit(root, &child)?;
            }
        }
        Ok(())
    }
    visit(root, root)
}

pub(crate) fn prepare_lower(root: &Path, android: bool) -> Result<()> {
    initialize_esp_permissions(root)?;
    if android {
        label_module_tree(root)?;
    }
    Ok(())
}

pub fn label_module(module: &Module) -> Result<()> {
    let root = Path::new(MODULES).join(&module.id);
    // Baselines already have canonical modes. Only fresh upper entries need
    // initialization; preserved generations retain the user's changed modes.
    if module.owner == Owner::Esp
        && !Path::new(STORE)
            .join("prepared")
            .join(&module.id)
            .join(&module.generation)
            .exists()
    {
        initialize_esp_permissions(&root)?;
    }
    label_module_tree(&root)?;
    label_tree(
        &Path::new(ROOT).join("state").join(&module.id),
        FILE_CONTEXT,
    )?;
    Ok(())
}
pub fn restore_efivarfs() -> Result<()> {
    // Only a loaded filesystem is restored; EFI is an independent critical LKM
    // when the platform requires it, not a generic ROM schema dependency.
    let available = fs::read_to_string("/proc/filesystems")?
        .lines()
        .any(|line| line.split_whitespace().last() == Some("efivarfs"));
    let path = Path::new("/sys/firmware/efi/efivars");
    if available && !fsu::mounted(path)? {
        fsu::mount(
            "efivarfs",
            path,
            "efivarfs",
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            &format!("context={FILE_CONTEXT}"),
        )?;
    }
    Ok(())
}

/// Stage `rd/` under the owned executable runtime, merging disjoint directories
/// but rejecting every file collision. `early_bin` lists module-relative files
/// copied by basename to bin. `rd_copy` lists `source:relative-destination`
/// entries below the owned rd tree. Neither can overwrite a bootstrap tool.
pub fn stage_module_files(module: &Module) -> Result<()> {
    fn plan(
        source: &Path,
        target: &Path,
        files: &mut Vec<(PathBuf, PathBuf, bool)>,
        seen: &mut BTreeMap<PathBuf, bool>,
        depth: usize,
    ) -> Result<()> {
        ensure!(depth < 128, "early staging nesting exceeds bound");
        let meta = fs::symlink_metadata(source)?;
        ensure!(!meta.file_type().is_symlink(), "symlink in early staging");
        if meta.is_dir() {
            match fs::symlink_metadata(target) {
                Ok(existing) => ensure!(
                    existing.is_dir() && !existing.file_type().is_symlink(),
                    "early directory collision"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("stat early staging directory"),
            }
            ensure!(
                seen.get(target) != Some(&false),
                "early directory collides with planned file"
            );
            if seen.insert(target.to_owned(), true).is_none() {
                files.push((source.to_owned(), target.to_owned(), true));
            }
            for child in fsu::children(source)? {
                plan(
                    &child,
                    &target.join(child.file_name().context("missing staged name")?),
                    files,
                    seen,
                    depth + 1,
                )?;
            }
        } else {
            ensure!(meta.is_file(), "invalid early staging file");
            match fs::symlink_metadata(target) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("stat early staging file"),
                Ok(_) => anyhow::bail!("early staging collision {}", target.display()),
            }
            ensure!(
                seen.insert(target.to_owned(), false).is_none(),
                "early staging collision {}",
                target.display()
            );
            files.push((source.to_owned(), target.to_owned(), false));
        }
        Ok(())
    }
    fn relative(root: &Path, name: &str) -> Result<PathBuf> {
        ensure!(
            !name.is_empty()
                && Path::new(name)
                    .components()
                    .all(|p| matches!(p, std::path::Component::Normal(_))),
            "unsafe early export path"
        );
        let path = root.join(name);
        let mut prefix = root.to_owned();
        for part in Path::new(name).components() {
            prefix.push(part);
            if let Ok(meta) = fs::symlink_metadata(&prefix) {
                ensure!(
                    !meta.file_type().is_symlink(),
                    "symlink in early export path"
                );
            }
        }
        Ok(path)
    }
    let root = Path::new(MODULES).join(&module.id);
    let rd = root.join("rd");
    let mut files = Vec::new();
    let mut seen = BTreeMap::new();
    match fs::symlink_metadata(&rd) {
        Ok(_) => plan(&rd, Path::new(RD), &mut files, &mut seen, 0)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("stat module rd exports"),
    }
    let prop = fsu::text(&root.join("module.prop"), 65536)?;
    for (key, values) in prop.lines().filter_map(|line| line.split_once('=')) {
        for item in values.split_whitespace() {
            match key.trim() {
                "early_bin" => {
                    let source = relative(&root, item)?;
                    plan(
                        &source,
                        &Path::new(BIN).join(source.file_name().context("invalid early_bin")?),
                        &mut files,
                        &mut seen,
                        0,
                    )?;
                }
                "rd_copy" => {
                    let (source, destination) = item
                        .split_once(':')
                        .context("rd_copy must be source:destination")?;
                    plan(
                        &relative(&root, source)?,
                        &relative(Path::new(RD), destination)?,
                        &mut files,
                        &mut seen,
                        0,
                    )?;
                }
                _ => {}
            }
        }
    }
    let mut copied = Vec::new();
    let mut directories = Vec::new();
    let result = (|| -> Result<()> {
        for (source, target, directory) in &files {
            if *directory {
                if !target.exists() {
                    fsu::directory(target)?;
                    directories.push(target);
                    fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
                }
            } else {
                fsu::directory(target.parent().context("missing staging parent")?)?;
                copied.push(target);
                fs::copy(source, target)?;
                fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for target in copied.iter().rev() {
            if target.exists() {
                fs::remove_file(target)
                    .with_context(|| format!("undo failed staging: {error:#}"))?;
            }
        }
        for target in directories.iter().rev() {
            fs::remove_dir(target)
                .with_context(|| format!("undo failed directory staging: {error:#}"))?;
        }
        return Err(error);
    }
    Ok(())
}
