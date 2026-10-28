//! Filesystem backing adaptation, not a lifecycle dispatcher. Magisk calls these
//! at PID1 preparation and daemon startup; no stage IPC or reconstruction service.
use crate::{bootstrap, context::*, esp, fsutil as fsu, store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
pub struct Storage {
    pub config: BootstrapConfig,
    pub mode: BootMode,
    pub debug: bool,
    esp: u64,
    backing: u64,
}

pub fn read() -> Result<Storage> {
    let storage: Storage =
        serde_json::from_str(&fsu::text(Path::new("/dev/egysk/storage.json"), 65536)?)?;
    storage.config.validate()?;
    Ok(storage)
}

pub fn open_early() -> Result<(Storage, Vec<Module>)> {
    let seed = crate::parse_bootstrap_config(&fsu::text(Path::new(BOOTSTRAP_CONFIG), 65536)?)?;
    let esp = esp::select(seed.esp_device.as_deref())?;
    let mut config = crate::parse_bootstrap_config(&fsu::text(Path::new(PACKAGE_CONFIG), 65536)?)?
        .for_boot(seed)?;
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    let bootconfig = fs::read_to_string("/proc/bootconfig").unwrap_or_default();
    config.safe_mode |= cmdline
        .split_whitespace()
        .any(|arg| arg == "egysk.safe_mode=1")
        || bootconfig
            .lines()
            .any(|line| line.trim() == "egysk.safe_mode = \"1\"");
    let mode = crate::boot_mode(&bootconfig, &cmdline, Path::new("/sbin/recovery").exists());
    let debug = crate::debug_boot(&bootconfig, &cmdline);
    fsu::directory(Path::new(BIN))?;
    bootstrap::stage_tools(&config)?;
    if config.backing.kind == "filesystem" {
        // Stock first-stage init owns this mount. No store, state, writable views
        // or module rdinit hooks exist yet; only package RC/policy is admitted.
        let storage = Storage {
            config,
            mode,
            debug,
            esp,
            backing: 0,
        };
        return Ok((storage, Vec::new()));
    }
    bootstrap::prepare_backing(&config)?;
    bootstrap::initialize_store()?;
    store::reclaim_lower_caches()?;
    if !config.safe_mode && mode != BootMode::Charger {
        store::activate_pending(mode, &config.backing.mount_at)?;
    }
    let modules = store::discover(&config, mode)?;
    store::mount_view(&modules, store::ViewStage::Rdinit)?;
    fsu::directory(Path::new(RD))?;
    for module in &modules {
        bootstrap::stage_module_files(module)?;
    }
    Ok((
        Storage {
            config,
            mode,
            debug,
            esp,
            backing: fs::metadata(STORE)?.dev(),
        },
        modules,
    ))
}

/// Keep only boot inputs and raw module RC/rules in upstream's RAM overlay.
/// There is no encoded inventory, stage progress or service schedule to replay.
pub fn save(storage: &Storage, modules: &[Module]) -> Result<()> {
    let overlay = Path::new("/egysk-overlay.d/sbin");
    fsu::directory(overlay)?;
    fsu::directory(&overlay.join("bin"))?;
    let mut tools = std::collections::BTreeSet::from(BOOTSTRAP_TOOLS);
    tools.extend(storage.config.backing.helper.as_deref());
    tools.extend(storage.config.tools.iter().map(String::as_str));
    for tool in tools {
        fs::copy(Path::new(BIN).join(tool), overlay.join("bin").join(tool))?;
    }
    std::os::unix::fs::symlink("egyskd", overlay.join("bin/resetprop"))?;
    fsu::atomic(&overlay.join("storage.json"), &serde_json::to_vec(storage)?)?;
    let mut rc = String::new();
    let mut policy = String::new();
    let package_modules;
    let modules = if storage.config.backing.kind == "filesystem" {
        package_modules = store::discover_package(&storage.config, storage.mode)?;
        &package_modules
    } else {
        modules
    };
    for module in modules {
        let root = if storage.config.backing.kind == "filesystem" {
            store::lower(module)
        } else {
            Path::new(MODULES).join(&module.id)
        };
        if let Some(rules) = fsu::optional_text(&root.join("sepolicy.rule"), 1024 * 1024)? {
            policy.push_str(&rules);
            policy.push('\n');
        }
        let initrc = root.join("initrc");
        if !storage.config.norc && initrc.exists() {
            for file in fsu::children(&initrc)? {
                if file.extension().and_then(|s| s.to_str()) == Some("rc") {
                    rc.push_str(&fsu::text(&file, 65536)?);
                    rc.push('\n');
                }
            }
        }
    }
    fsu::atomic(
        Path::new("/egysk-overlay.d/egysk-modules.rc"),
        rc.as_bytes(),
    )?;
    fsu::atomic(&overlay.join("module-policy"), policy.as_bytes())
}

pub fn open_android() -> Result<()> {
    let storage = read()?;
    esp::mount(storage.esp)?;
    // The helper activates and adopts the configured store again. Its volume
    // selector is not a carried device node: the mapping may be recreated with
    // a different number across the Android root transition.
    let config = storage.config;
    bootstrap::prepare_backing(&config)?;
    if config.backing.kind == "block" {
        ensure!(
            fs::metadata(STORE)?.dev() == storage.backing,
            "shared store changed during boot"
        );
    }
    bootstrap::initialize_store()?;
    // Only PID 1 reclaims on-disk lower caches under store/overlays. Filesystem
    // backing is first available here, after stock init starts, so keep those
    // caches. Activation still checks that no effective module views are mounted.
    if config.backing.kind == "filesystem" && !config.safe_mode && storage.mode != BootMode::Charger
    {
        store::activate_pending(storage.mode, &config.backing.mount_at)?;
    }
    bootstrap::label_tree(Path::new(STORE), FILE_CONTEXT)?;
    let modules = store::discover(&config, storage.mode)?;
    store::mount_view(&modules, store::ViewStage::Android)?;
    fsu::directory(Path::new(RD))?;
    for module in &modules {
        bootstrap::stage_module_files(module)?;
        bootstrap::label_module(module)?;
        fsu::atomic(
            &Path::new(STORE)
                .join("prepared")
                .join(&module.id)
                .join(&module.generation),
            b"1",
        )?;
    }
    bootstrap::label_tree(Path::new(RD), FILE_CONTEXT)?;
    bootstrap::label_tree(Path::new(BIN), FILE_CONTEXT)?;
    bootstrap::restore_efivarfs()
}

pub fn mount_snapshot() -> Result<Vec<(u64, PathBuf)>> {
    fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let id = fields.next().context("mount ID")?.parse()?;
            let path = fields.nth(3).context("mount target")?;
            Ok((
                id,
                PathBuf::from(
                    path.replace("\\040", " ")
                        .replace("\\011", "\t")
                        .replace("\\012", "\n")
                        .replace("\\134", "\\"),
                ),
            ))
        })
        .collect()
}

pub fn close_early(before: &[(u64, PathBuf)]) -> Result<()> {
    std::env::set_current_dir("/")?;
    let mut mounts = mount_snapshot()?
        .into_iter()
        .filter(|(id, _)| !before.iter().any(|(old, _)| old == id))
        .collect::<Vec<_>>();
    mounts.sort_by_key(|(id, path)| {
        (
            std::cmp::Reverse(path.components().count()),
            std::cmp::Reverse(*id),
        )
    });
    for (_, path) in mounts {
        if let Err(error) = fsu::unmount(&path) {
            // Platform loop mappings legitimately retain raw ESP files. No view,
            // upper or metadata mount may be detached to conceal a busy failure.
            let retained = path == Path::new(ESP)
                && fsu::children(Path::new("/sys/block"))?.iter().any(|p| {
                    fs::read_to_string(p.join("loop/backing_file"))
                        .is_ok_and(|v| Path::new(v.trim()).starts_with(ESP))
                });
            ensure!(
                retained
                    && error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.raw_os_error() == Some(libc::EBUSY)),
                "unmount {}: {error:#}",
                path.display()
            );
            ensure!(
                unsafe { libc::umount2(c"/dev/esp".as_ptr(), libc::MNT_DETACH) } == 0,
                "detach retained ESP: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(())
}
