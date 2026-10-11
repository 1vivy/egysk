//! Shared package ownership and recoverable activation. All lower renames happen
//! before mounting. A synced journal precedes each multi-directory transaction;
//! recovery uses content identity, never the mere presence of a directory.
use crate::context::*;
use crate::fsutil as fsu;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub fn valued_flag(root: &Path, name: &str) -> Result<bool> {
    if let Some(value) = fsu::optional_text(&root.join(name), 64)? {
        return Ok(matches!(value.trim(), "1" | "true")
            || (value.trim().is_empty() && matches!(name, "disable" | "remove" | "skip_mount")));
    }
    let Some(prop) = fsu::optional_text(&root.join("module.prop"), 65536)? else {
        return Ok(false);
    };
    let mut values = prop
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim());
    let first = values.next();
    ensure!(values.next().is_none(), "duplicate module flag {name}");
    Ok(matches!(first, Some("1" | "true")))
}
pub fn identify(root: &Path, id: &str) -> Result<()> {
    identifier(id)?;
    let prop = fsu::text(&root.join("module.prop"), 65536)?;
    let mut ids = prop
        .lines()
        .filter_map(|s| s.split_once('='))
        .filter(|(k, _)| k.trim() == "id")
        .map(|(_, v)| v.trim());
    ensure!(
        ids.next() == Some(id) && ids.next().is_none(),
        "module.prop ID mismatch: {id}"
    );
    Ok(())
}
fn owner_root(owner: Owner) -> &'static Path {
    Path::new(match owner {
        Owner::Esp => PACKAGE,
        Owner::Local => STORE,
    })
}
pub fn lower(module: &Module) -> PathBuf {
    owner_root(module.owner).join("modules").join(&module.id)
}
fn current_generation(path: &Path, owner: Owner) -> Result<String> {
    if owner == Owner::Local
        && let Some(generation) = fsu::optional_text(&path.join(GENERATION_MARKER), 64)?
    {
        ensure!(
            generation.len() == 64 && generation.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid local generation marker"
        );
        return Ok(generation);
    }
    let generation = fsu::generation(path)?;
    // Manual local installs still use directory+module.prop inventory. Record
    // their initial package identity before any script mutates the writable tree.
    if owner == Owner::Local {
        fsu::atomic(&path.join(GENERATION_MARKER), generation.as_bytes())?;
    }
    Ok(generation)
}
fn inventory_root(root: &Path) -> Result<Vec<String>> {
    fsu::directory(root)?;
    let mut ids = Vec::new();
    for path in fsu::children(root)? {
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "invalid module directory {}",
            path.display()
        );
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("non UTF-8 module ID")?
            .to_owned();
        identifier(&id)?;
        ids.push(id);
    }
    Ok(ids)
}
pub fn discover(config: &BootstrapConfig, mode: BootMode) -> Result<Vec<Module>> {
    if config.safe_mode || mode == BootMode::Charger {
        return Ok(Vec::new());
    }
    let mut all = BTreeMap::new();
    for owner in [Owner::Esp, Owner::Local] {
        for id in inventory_root(&owner_root(owner).join("modules"))? {
            ensure!(
                all.insert(id.clone(), owner).is_none(),
                "module {id} belongs to both ESP and local storage"
            );
        }
    }
    let precedence =
        fsu::optional_text(&Path::new(PACKAGE).join("modules_order"), 65536)?.unwrap_or_default();
    let order = ordered_ids(&all, &precedence)?;
    let mut result = Vec::new();
    for id in order {
        let owner = all[&id];
        let directory = owner_root(owner).join("modules").join(&id);
        let preferences = Path::new(STORE).join("preferences").join(&id);
        let overridden = |flag: &str| -> Result<bool> {
            match fsu::optional_text(&preferences.join(flag), 64)? {
                Some(value) => Ok(matches!(value.trim(), "1" | "true")),
                None => valued_flag(&directory, flag),
            }
        };
        if overridden("disable")? || overridden("remove")? {
            continue;
        }
        if mode == BootMode::Recovery && !valued_flag(&directory, "recovery_ok")? {
            continue;
        }
        identify(&directory, &id)?;
        result.push(Module {
            generation: current_generation(&directory, owner)?,
            id,
            owner,
        });
    }
    Ok(result)
}

fn ordered_ids(all: &BTreeMap<String, Owner>, precedence: &str) -> Result<Vec<String>> {
    let mut order = Vec::with_capacity(all.len());
    let mut seen = BTreeSet::new();
    for id in precedence
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        identifier(id)?;
        ensure!(
            seen.insert(id.to_owned()),
            "duplicate modules_order entry {id}"
        );
        if all.contains_key(id) {
            order.push(id.to_owned());
        }
    }
    order.extend(all.keys().filter(|id| !seen.contains(*id)).cloned());
    Ok(order)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    id: String,
    owner: Owner,
    old_owner: Option<Owner>,
    old: Option<String>,
    new: String,
    /// `update` or a previous generation hash for an explicit rollback.
    source: String,
}
fn journal(id: &str) -> PathBuf {
    Path::new(STORE)
        .join("activations")
        .join(format!("{id}.json"))
}
fn verify_generation(path: &Path, expected: &str, owner: Owner) -> Result<()> {
    ensure!(
        current_generation(path, owner)? == expected,
        "generation mismatch at {}",
        path.display()
    );
    Ok(())
}
fn installed_owner(id: &str) -> Result<Option<Owner>> {
    let mut selected = None;
    for owner in [Owner::Esp, Owner::Local] {
        if owner_root(owner).join("modules").join(id).exists() {
            ensure!(
                selected.replace(owner).is_none(),
                "duplicate installed ownership for {id}"
            );
        }
    }
    Ok(selected)
}
fn recover(transaction: &Activation) -> Result<()> {
    recover_at(
        transaction,
        owner_root(transaction.owner),
        owner_root(transaction.old_owner.unwrap_or(transaction.owner)),
        &journal(&transaction.id),
    )
}
fn recover_at(
    transaction: &Activation,
    root: &Path,
    old_root: &Path,
    journal_path: &Path,
) -> Result<()> {
    identifier(&transaction.id)?;
    ensure!(
        transaction.old.is_some() == transaction.old_owner.is_some(),
        "invalid activation old ownership"
    );
    ensure!(
        transaction.new.len() == 64 && transaction.new.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid activation generation"
    );
    let installed = root.join("modules").join(&transaction.id);
    let old_installed = old_root.join("modules").join(&transaction.id);
    let source = if transaction.source == "update" {
        root.join("modules_update").join(&transaction.id)
    } else {
        ensure!(
            transaction.source.len() == 64
                && transaction.source.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid rollback generation"
        );
        root.join("modules_previous")
            .join(&transaction.id)
            .join(&transaction.source)
    };
    if installed.exists()
        && current_generation(&installed, transaction.owner)? == transaction.new
        && !source.exists()
    {
        ensure!(
            root == old_root || !old_installed.exists(),
            "activation retained duplicate installed ownership"
        );
    } else {
        verify_generation(&source, &transaction.new, transaction.owner)?;
        if let (Some(old), Some(old_owner)) = (&transaction.old, transaction.old_owner) {
            ensure!(
                old.len() == 64 && old.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid old generation"
            );
            let previous = old_root
                .join("modules_previous")
                .join(&transaction.id)
                .join(old);
            if old_installed.exists() {
                verify_generation(&old_installed, old, old_owner)?;
                ensure!(!previous.exists(), "rollback destination already occupied");
                fsu::rename(&old_installed, &previous)?;
            } else {
                verify_generation(&previous, old, old_owner)?;
            }
        }
        ensure!(
            !installed.exists(),
            "unexpected installed lower during activation"
        );
        fsu::rename(&source, &installed)?;
    }
    verify_generation(&installed, &transaction.new, transaction.owner)?;
    fsu::sync(&installed)?;
    fs::remove_file(journal_path)?;
    fsu::sync(journal_path.parent().context("missing journal parent")?)
}
fn begin(id: &str, owner: Owner, source: &str, staged: &Path) -> Result<()> {
    identify(staged, id)?;
    let new = fsu::generation(staged)?;
    if owner == Owner::Local {
        fsu::atomic(&staged.join(GENERATION_MARKER), new.as_bytes())?;
    }
    fsu::sync_tree(staged)?;
    let old_owner = installed_owner(id)?;
    let old = old_owner
        .map(|old_owner| {
            let path = owner_root(old_owner).join("modules").join(id);
            let generation = current_generation(&path, old_owner)?;
            fsu::sync_tree(&path)?;
            Ok::<_, anyhow::Error>(generation)
        })
        .transpose()?;
    if old.as_ref() == Some(&new) && old_owner == Some(owner) {
        // Reinstalling unchanged bits must retain the existing upper/config.
        fs::remove_dir_all(staged)?;
        return fsu::sync(staged.parent().context("missing update parent")?);
    }
    ensure!(
        !owner_root(owner)
            .join("modules_previous")
            .join(id)
            .join(&new)
            .exists(),
        "generation already retained; use explicit rollback instead of a duplicate update"
    );
    let transaction = Activation {
        id: id.to_owned(),
        owner,
        old_owner,
        old,
        new,
        source: source.to_owned(),
    };
    fsu::atomic(&journal(id), &serde_json::to_vec(&transaction)?)?;
    recover(&transaction)
}
/// Called only in rdinit, before *any* module view is mounted. Never from prepare().
pub fn activate_pending(mode: BootMode, backing_root: &str) -> Result<()> {
    fsu::directory(Path::new(STORE))?;
    let _lock = fsu::StoreLock::acquire(Path::new(STORE))?;
    for path in [
        "activations",
        "preferences",
        "state",
        "overlays",
        "modules",
        "modules_update",
        "modules_previous",
    ] {
        fsu::directory(&Path::new(STORE).join(path))?;
    }
    ensure_views_unmounted()?;
    for path in fsu::children(&Path::new(STORE).join("activations"))? {
        if path.extension().and_then(|value| value.to_str()) == Some(TRANSACTION_TEMP_EXTENSION) {
            continue;
        }
        let transaction: Activation = serde_json::from_str(&fsu::text(&path, 4096)?)?;
        ensure!(
            path == journal(&transaction.id),
            "activation journal name mismatch"
        );
        recover(&transaction)?;
    }
    for owner in [Owner::Esp, Owner::Local] {
        let updates = owner_root(owner).join("modules_update");
        fsu::directory(&updates)?;
        for id in inventory_root(&updates)? {
            let other = owner_root(if owner == Owner::Esp {
                Owner::Local
            } else {
                Owner::Esp
            })
            .join("modules_update")
            .join(&id);
            ensure!(
                !other.exists(),
                "duplicate pending update ownership for {id}"
            );
            begin(&id, owner, "update", &updates.join(&id))?;
        }
    }
    // Removed packages never run Android uninstall work in recovery/charger.
    if mode != BootMode::Normal {
        return Ok(());
    }
    for path in fsu::children(&Path::new(STORE).join("preferences"))? {
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("invalid preference ID")?;
        identifier(id)?;
        if valued_flag(&path, "remove")? && !valued_flag(&path, "uninstalled")? {
            // Uninstall sees its last effective upper/state, not the raw lower.
            // A power loss before the completion marker may rerun uninstall;
            // scripts must tolerate that, as with upstream boot-time uninstall.
            let mut candidates = [Owner::Esp, Owner::Local]
                .into_iter()
                .filter_map(|owner| {
                    let lower = owner_root(owner).join("modules").join(id);
                    lower.exists().then_some((owner, lower))
                })
                .collect::<Vec<_>>();
            ensure!(
                candidates.len() <= 1,
                "duplicate ownership during uninstall"
            );
            if let Some((owner, lower)) = candidates.pop() {
                let module = Module {
                    id: id.to_owned(),
                    generation: current_generation(&lower, owner)?,
                    owner,
                };
                let mounts = mount_view(std::slice::from_ref(&module), ViewStage::Rdinit)?;
                let result = crate::scripts::run_one(
                    &module,
                    &Path::new(MODULES).join(id),
                    "uninstall",
                    BootMode::Normal,
                    backing_root,
                    std::time::Instant::now() + std::time::Duration::from_secs(35),
                );
                for mount in mounts.iter().rev() {
                    fsu::unmount(mount)?;
                }
                result?;
            }
            fsu::atomic(&path.join("uninstalled"), b"1")?;
        }
    }
    Ok(())
}

pub fn stage_install(id: &str, unpacked: &Path) -> Result<()> {
    fsu::root_only()?;
    identifier(id)?;
    identify(unpacked, id)?;
    let _lock = fsu::StoreLock::acquire(Path::new(STORE))?;
    // Native customization needs Unix modes/symlinks. Replacing an ESP ID
    // therefore queues an ownership transfer, retaining its old ESP lower and
    // upper for rollback. Installed ownership never overlaps.
    installed_owner(id)?;
    let owner = Owner::Local;
    ensure!(
        !Path::new(PACKAGE).join("modules_update").join(id).exists(),
        "ESP update already pending"
    );
    let updates = Path::new(STORE).join("modules_update");
    fsu::directory(&updates)?;
    let target = updates.join(id);
    ensure!(
        !target.exists() && !journal(id).exists(),
        "module already has a pending transaction"
    );
    let temporary = owner_root(owner).join(format!(".install-{id}"));
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
        fsu::sync(owner_root(owner))?;
    }
    fsu::copy_tree(unpacked, &temporary)?;
    let generation = fsu::generation(&temporary)?;
    ensure!(
        !Path::new(STORE)
            .join("modules_previous")
            .join(id)
            .join(&generation)
            .exists(),
        "generation already retained; use explicit rollback"
    );
    fsu::atomic(&temporary.join(GENERATION_MARKER), generation.as_bytes())?;
    fsu::rename(&temporary, &target)?;
    fsu::atomic(
        &Path::new(STORE).join("preferences").join(id).join("remove"),
        b"0",
    )
}
fn preference(id: &str, name: &str, value: bool) -> Result<()> {
    fsu::root_only()?;
    identifier(id)?;
    let _lock = fsu::StoreLock::acquire(Path::new(STORE))?;
    fsu::atomic(
        &Path::new(STORE).join("preferences").join(id).join(name),
        if value { b"1" } else { b"0" },
    )
}
pub fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    preference(id, "disable", !enabled)
}
pub fn request_remove(id: &str) -> Result<()> {
    fsu::root_only()?;
    identifier(id)?;
    let _lock = fsu::StoreLock::acquire(Path::new(STORE))?;
    let preferences = Path::new(STORE).join("preferences").join(id);
    fsu::atomic(&preferences.join("uninstalled"), b"0")?;
    fsu::atomic(&preferences.join("remove"), b"1")
}
pub fn cancel_remove(id: &str) -> Result<()> {
    preference(id, "remove", false)
}
pub fn request_remove_all() -> Result<()> {
    fsu::root_only()?;
    for owner in [Owner::Esp, Owner::Local] {
        for id in inventory_root(&owner_root(owner).join("modules"))? {
            request_remove(&id)?;
        }
    }
    Ok(())
}
/// Queue rollback, never rename a lower behind an active overlay. Recovery next
/// rdinit keeps the abandoned current generation and restores the old upper pair.
pub fn rollback(id: &str, generation: &str) -> Result<()> {
    fsu::root_only()?;
    identifier(id)?;
    ensure!(
        generation.len() == 64 && generation.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid rollback generation"
    );
    let _lock = fsu::StoreLock::acquire(Path::new(STORE))?;
    ensure!(
        !journal(id).exists()
            && [Owner::Esp, Owner::Local]
                .iter()
                .all(|owner| !owner_root(*owner).join("modules_update").join(id).exists()),
        "pending module activation"
    );
    let old_owner = installed_owner(id)?.context("module not installed")?;
    let candidates = [Owner::Esp, Owner::Local]
        .into_iter()
        .filter(|owner| {
            owner_root(*owner)
                .join("modules_previous")
                .join(id)
                .join(generation)
                .exists()
        })
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 1,
        "rollback generation is missing or ambiguous"
    );
    let owner = candidates[0];
    let source = owner_root(owner)
        .join("modules_previous")
        .join(id)
        .join(generation);
    identify(&source, id)?;
    verify_generation(&source, generation, owner)?;
    let old = current_generation(&owner_root(old_owner).join("modules").join(id), old_owner)?;
    ensure!(
        old != generation || old_owner != owner,
        "already selected generation"
    );
    let transaction = Activation {
        id: id.to_owned(),
        owner,
        old_owner: Some(old_owner),
        old: Some(old),
        new: generation.to_owned(),
        source: generation.to_owned(),
    };
    fsu::atomic(&journal(id), &serde_json::to_vec(&transaction)?)
}

fn ensure_views_unmounted() -> Result<()> {
    let modules = Path::new(MODULES);
    ensure!(
        !fsu::mounted(Path::new(LOWERS))?
            && !fsu::mounted(modules)?
            && (!modules.try_exists()?
                || fsu::children(modules)?
                    .iter()
                    .all(|path| !fsu::mounted(path).unwrap_or(true))),
        "storage mutation forbidden while effective modules are mounted"
    );
    Ok(())
}

/// Reclaim only generated baseline copies, before any boot consumers exist.
/// Upper/work, local packages, preferences and durable state are never removed.
pub fn reclaim_lower_caches() -> Result<()> {
    // SAFETY: getpid has no pointer arguments or memory preconditions.
    ensure!(
        unsafe { libc::getpid() } == 1,
        "lower-cache reclamation requires PID1"
    );
    ensure_views_unmounted()?;
    reclaim_lower_caches_at(&Path::new(STORE).join("overlays"))
}

fn reclaim_lower_caches_at(overlays: &Path) -> Result<()> {
    if !overlays.try_exists()? {
        return Ok(());
    }
    fsu::directory(overlays)?;
    let mut caches = Vec::new();
    for module in fsu::children(overlays)? {
        identifier(
            module
                .file_name()
                .and_then(|s| s.to_str())
                .context("invalid cache module ID")?,
        )?;
        fsu::directory(&module)?;
        for generation in fsu::children(&module)? {
            let expected = generation
                .file_name()
                .and_then(|s| s.to_str())
                .context("invalid cache generation")?;
            ensure!(
                expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid cache generation"
            );
            fsu::directory(&generation)?;
            for name in [".lower-incomplete", "lower"] {
                let cache = generation.join(name);
                let metadata = match fs::symlink_metadata(&cache) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "unsafe generated lower cache: {}",
                    cache.display()
                );
                if name == "lower" {
                    verify_generation(&cache, expected, Owner::Esp)?;
                }
                caches.push(cache);
            }
        }
    }
    // Validate all published copies before deleting any: unexpected contents
    // may be unique user data, not a reconstructible baseline.
    for cache in caches {
        let parent = cache.parent().context("cache parent missing")?;
        let retired = parent.join(".lower-incomplete");
        if cache != retired {
            // Persist retirement before unlinking anything. Interrupted deletion
            // must not leave a partial tree under a verified, published name.
            fsu::rename(&cache, &retired)?;
        }
        fs::remove_dir_all(&retired)?;
        fsu::sync(parent)?;
    }
    Ok(())
}

fn prepare_esp_lower(source: &Path, generation: &Path, expected: &str) -> Result<PathBuf> {
    fsu::directory(generation)?;
    let lower = generation.join("lower");
    if lower.try_exists()? {
        verify_generation(&lower, expected, Owner::Esp)?;
        return Ok(lower);
    }
    // VFAT dentries cannot be OverlayFS lowers. Materialize a verified Unix
    // snapshot in the owned boot-local tmpfs, never on persistent metadata.
    // Never expose a partial copy or replace a lower after publication.
    let pending = generation.join(".lower-incomplete");
    if pending.try_exists()? {
        let metadata = fs::symlink_metadata(&pending)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "unsafe incomplete lower snapshot"
        );
        fs::remove_dir_all(&pending)?;
    }
    fsu::copy_tree(source, &pending)?;
    verify_generation(&pending, expected, Owner::Esp)?;
    fsu::rename(&pending, &lower)?;
    Ok(lower)
}

fn require_metacopy(modules: &[Module]) -> Result<()> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let mut default = None;
    for module in modules.iter().filter(|module| module.owner == Owner::Esp) {
        let line = mountinfo
            .lines()
            .find(|line| {
                line.split_whitespace().nth(4).is_some_and(|target| {
                    let target = Path::new(target);
                    target.parent() == Some(Path::new(MODULES))
                        && target.file_name() == Some(std::ffi::OsStr::new(&module.id))
                })
            })
            .context("effective module mount missing")?;
        let mut fields = line
            .split_once(" - ")
            .context("invalid module mountinfo")?
            .1
            .split_whitespace();
        ensure!(
            fields.next() == Some("overlay"),
            "module view is not OverlayFS"
        );
        let options = fields.nth(1).context("module superblock options missing")?;
        ensure!(
            !options.split(',').any(|option| option == "userxattr"),
            "module overlay requires trusted metadata"
        );
        let enabled = match options
            .split(',')
            .find_map(|option| option.strip_prefix("metacopy="))
        {
            Some("on") => true,
            Some("off") => false,
            Some(_) => anyhow::bail!("unknown OverlayFS metacopy mode"),
            None => {
                // show_options omits values equal to the current module default.
                // A successful mount alone does not rule out a silent downgrade.
                if default.is_none() {
                    let value =
                        fsu::text(Path::new("/sys/module/overlay/parameters/metacopy"), 16)?;
                    default = Some(match value.trim() {
                        "Y" => true,
                        "N" => false,
                        _ => anyhow::bail!("unknown OverlayFS metacopy default"),
                    });
                }
                default.context("OverlayFS metacopy default missing")?
            }
        };
        ensure!(
            enabled,
            "module {} lost metadata-only copy-up support",
            module.id
        );
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewStage {
    Rdinit,
    Android,
}

pub fn mount_view(modules: &[Module], stage: ViewStage) -> Result<Vec<PathBuf>> {
    let mut owned = Vec::new();
    let result = (|| {
        fsu::directory(Path::new(MODULES))?;
        ensure_views_unmounted()?;
        let has_esp = modules.iter().any(|module| module.owner == Owner::Esp);
        if has_esp {
            fsu::mount(
                MOUNT_SOURCE,
                Path::new(LOWERS),
                EXECUTABLE_MOUNT.fs_type,
                EXECUTABLE_MOUNT.flags,
                EXECUTABLE_MOUNT.data,
            )?;
            owned.push(PathBuf::from(LOWERS));
        }
        for module in modules {
            let lower = lower(module);
            identify(&lower, &module.id)?;
            verify_generation(&lower, &module.generation, module.owner)?;
            let target = Path::new(MODULES).join(&module.id);
            ensure!(!fsu::mounted(&target)?, "effective view already mounted");
            if module.owner == Owner::Esp {
                let generation = Path::new(STORE)
                    .join("overlays")
                    .join(&module.id)
                    .join(&module.generation);
                let staging = Path::new(LOWERS).join(&module.id).join(&module.generation);
                let lower = prepare_esp_lower(&lower, &staging, &module.generation)?;
                crate::bootstrap::prepare_lower(&lower, stage == ViewStage::Android)?;
                let upper = generation.join("upper");
                let work = generation.join("work");
                fsu::directory(&upper)?;
                fsu::directory(&work)?;
                // Refuse silent metacopy downgrades before exposing any views.
                fsu::mount(
                    "egysk",
                    &target,
                    "overlay",
                    libc::MS_NODEV | libc::MS_NOSUID,
                    &format!(
                        "lowerdir={},upperdir={},workdir={},index=off,metacopy=on,xino=off,redirect_dir=on,nfs_export=off",
                        lower.display(),
                        upper.display(),
                        work.display()
                    ),
                )?;
            } else {
                fsu::mount(
                    lower.to_str().context("invalid local path")?,
                    &target,
                    "",
                    libc::MS_BIND,
                    "",
                )?;
                owned.push(target.clone());
                fsu::mount(
                    "",
                    &target,
                    "",
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_NOSUID | libc::MS_NODEV,
                    "",
                )?;
                owned.pop();
            }
            owned.push(target);
            let state = Path::new(STORE).join("state").join(&module.id);
            fsu::directory(&state)?;
            let target = Path::new(ROOT).join("state").join(&module.id);
            fsu::mount(
                state.to_str().context("invalid state path")?,
                &target,
                "",
                libc::MS_BIND,
                "",
            )?;
            owned.push(target);
        }
        if has_esp {
            require_metacopy(modules)?;
            // All baseline writes finish before scripts or native projection.
            // Persistent changes must go through the generation's upper.
            fsu::mount(
                "",
                Path::new(LOWERS),
                "",
                libc::MS_REMOUNT | libc::MS_RDONLY | EXECUTABLE_MOUNT.flags,
                EXECUTABLE_MOUNT.data,
            )?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in owned.iter().rev() {
            fsu::unmount(path).with_context(|| format!("cleanup after {error:#}"))?;
        }
        return Err(error);
    }
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, contents: &str) -> String {
        fsu::directory(root).unwrap();
        fs::write(root.join("module.prop"), "id=example\n").unwrap();
        fs::write(root.join("payload"), contents).unwrap();
        fsu::generation(root).unwrap()
    }

    #[test]
    fn lower_snapshot_recovers_incomplete_copy_without_replacing_published_data() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("package");
        let generation = package(&source, "original");
        let storage = temporary.path().join("generation");
        let pending = storage.join(".lower-incomplete");
        package(&pending, "interrupted copy");
        fs::write(pending.join("stale"), "must not survive").unwrap();

        let lower = prepare_esp_lower(&source, &storage, &generation).unwrap();
        assert_eq!(
            fs::read_to_string(lower.join("payload")).unwrap(),
            "original"
        );
        assert!(!lower.join("stale").exists());
        assert!(!pending.exists());
        assert_eq!(fsu::generation(&lower).unwrap(), generation);

        fs::write(source.join("payload"), "changed after admission").unwrap();
        prepare_esp_lower(&source, &storage, &generation).unwrap();
        assert_eq!(
            fs::read_to_string(lower.join("payload")).unwrap(),
            "original"
        );
        let unpublished = temporary.path().join("unpublished");
        assert!(prepare_esp_lower(&source, &unpublished, &generation).is_err());
        assert!(!unpublished.join("lower").exists());

        fs::write(lower.join("payload"), "corrupt cached lower").unwrap();
        assert!(prepare_esp_lower(&source, &storage, &generation).is_err());
        assert_eq!(
            fs::read_to_string(lower.join("payload")).unwrap(),
            "corrupt cached lower"
        );
    }

    #[test]
    fn cache_reclamation_preserves_upper_work_and_durable_state() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let expected = package(&source, "baseline");
        let store = temporary.path().join("store");
        let overlays = store.join("overlays");
        let generation = overlays.join("example").join(expected);
        fsu::copy_tree(&source, &generation.join("lower")).unwrap();
        package(&generation.join(".lower-incomplete"), "partial");
        let retained = [
            (generation.join("upper/payload"), "user edits"),
            (generation.join("work/retained"), "work state"),
            (store.join("state/example/durable"), "module state"),
            (store.join("modules/local/payload"), "local install"),
            (temporary.path().join("password_slots/slot_map"), "0=gsi2"),
        ];
        for (path, contents) in &retained {
            fsu::directory(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        reclaim_lower_caches_at(&overlays).unwrap();
        assert!(!generation.join("lower").exists());
        assert!(!generation.join(".lower-incomplete").exists());
        for (path, contents) in &retained {
            assert_eq!(fs::read_to_string(path).unwrap(), *contents);
        }
    }

    #[test]
    fn failed_cache_deletion_leaves_only_a_resumable_incomplete_copy() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid takes no arguments. Root bypasses the permission fault.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        package(&source, "baseline");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o555)).unwrap();
        let expected = fsu::generation(&source).unwrap();
        let overlays = temporary.path().join("overlays");
        let generation = overlays.join("example").join(expected);
        let lower = generation.join("lower");
        let incomplete = generation.join(".lower-incomplete");
        fsu::copy_tree(&source, &lower).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        let result = reclaim_lower_caches_at(&overlays);
        // Restore permissions even on a regression so the fixture cleans up.
        for path in [&lower, &incomplete] {
            if path.exists() {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(!lower.exists(), "failed deletion left a published lower");
        assert!(incomplete.is_dir());
        fs::remove_file(incomplete.join("payload")).unwrap();
        reclaim_lower_caches_at(&overlays).unwrap();
        assert!(!incomplete.exists());
    }

    #[test]
    fn cache_reclamation_checks_every_published_copy_before_deleting_any() {
        let temporary = tempfile::tempdir().unwrap();
        let overlays = temporary.path().join("overlays");
        let mut generations = Vec::new();
        for contents in ["first", "second"] {
            let source = temporary.path().join(contents);
            let expected = package(&source, contents);
            let generation = overlays.join("example").join(expected);
            fsu::copy_tree(&source, &generation.join("lower")).unwrap();
            generations.push(generation);
        }
        generations.sort();
        let changed = generations[1].join("lower/payload");
        fs::write(&changed, "unexpected unique data").unwrap();
        assert!(reclaim_lower_caches_at(&overlays).is_err());
        for generation in generations {
            assert!(generation.join("lower/payload").is_file());
        }
        assert_eq!(
            fs::read_to_string(changed).unwrap(),
            "unexpected unique data"
        );
    }

    #[test]
    fn cache_reclamation_refuses_a_link_to_an_unrelated_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let unrelated = temporary.path().join("unrelated");
        let expected = package(&unrelated, "must survive");
        let overlays = temporary.path().join("overlays");
        let generation = overlays.join("example").join(expected);
        fsu::directory(&generation).unwrap();
        std::os::unix::fs::symlink(&unrelated, generation.join(".lower-incomplete")).unwrap();
        assert!(reclaim_lower_caches_at(&overlays).is_err());
        assert_eq!(
            fs::read_to_string(unrelated.join("payload")).unwrap(),
            "must survive"
        );
        assert!(generation.join(".lower-incomplete").is_symlink());
    }

    #[test]
    fn interrupted_activation_and_cross_owner_rollback_keep_matching_pairs() {
        for interruption in 0..=2 {
            let temporary = tempfile::tempdir().unwrap();
            let esp = temporary.path().join("esp");
            let metadata = temporary.path().join("metadata");
            let old_installed = esp.join("modules/example");
            let staged = metadata.join("modules_update/example");
            let installed = metadata.join("modules/example");
            let old = package(&old_installed, "old");
            let new = package(&staged, "new");
            let old_upper = metadata.join("overlays/example").join(&old).join("upper");
            fsu::directory(&old_upper).unwrap();
            fs::write(old_upper.join("copied-up"), "user edits").unwrap();
            let state = metadata.join("state/example");
            fsu::directory(&state).unwrap();
            fs::write(state.join("durable"), "shared state").unwrap();
            let transaction = Activation {
                id: "example".into(),
                owner: Owner::Local,
                old_owner: Some(Owner::Esp),
                old: Some(old.clone()),
                new: new.clone(),
                source: "update".into(),
            };
            let journal = metadata.join("activations/example.json");
            fsu::atomic(&journal, &serde_json::to_vec(&transaction).unwrap()).unwrap();
            let previous = esp.join("modules_previous/example").join(&old);
            if interruption >= 1 {
                fsu::rename(&old_installed, &previous).unwrap();
            }
            if interruption >= 2 {
                fsu::rename(&staged, &installed).unwrap();
            }
            recover_at(&transaction, &metadata, &esp, &journal).unwrap();
            assert!(!old_installed.exists() && !journal.exists());
            assert_eq!(fsu::generation(&installed).unwrap(), new);
            assert_eq!(fsu::generation(&previous).unwrap(), old);
            assert_eq!(
                fs::read_to_string(old_upper.join("copied-up")).unwrap(),
                "user edits"
            );
            assert_eq!(
                fs::read_to_string(state.join("durable")).unwrap(),
                "shared state"
            );

            let rollback = Activation {
                id: "example".into(),
                owner: Owner::Esp,
                old_owner: Some(Owner::Local),
                old: Some(new.clone()),
                new: old.clone(),
                source: old.clone(),
            };
            fsu::atomic(&journal, &serde_json::to_vec(&rollback).unwrap()).unwrap();
            recover_at(&rollback, &esp, &metadata, &journal).unwrap();
            assert!(!installed.exists());
            assert_eq!(fsu::generation(&old_installed).unwrap(), old);
            assert_eq!(
                fsu::generation(&metadata.join("modules_previous/example").join(new)).unwrap(),
                transaction.new
            );
            assert_eq!(
                fs::read_to_string(old_upper.join("copied-up")).unwrap(),
                "user edits"
            );
        }
    }

    #[test]
    fn recovery_rejects_a_mismatched_staged_generation_without_moving_old() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let old = package(&root.join("modules/example"), "old");
        package(&root.join("modules_update/example"), "tampered");
        let transaction = Activation {
            id: "example".into(),
            owner: Owner::Esp,
            old_owner: Some(Owner::Esp),
            old: Some(old.clone()),
            new: "ab".repeat(32),
            source: "update".into(),
        };
        let journal = root.join("activations/example.json");
        fsu::atomic(&journal, &serde_json::to_vec(&transaction).unwrap()).unwrap();
        assert!(recover_at(&transaction, root, root, &journal).is_err());
        assert_eq!(fsu::generation(&root.join("modules/example")).unwrap(), old);
        assert!(journal.exists());
    }

    #[test]
    fn root_order_ranks_inventory_and_appends_unlisted_ids_stably() {
        let inventory = BTreeMap::from([
            ("alpha".to_owned(), Owner::Local),
            ("middle".to_owned(), Owner::Esp),
            ("zeta".to_owned(), Owner::Esp),
        ]);
        assert_eq!(
            ordered_ids(
                &inventory,
                "# ranking, not inventory\nzeta\nnot-installed\n"
            )
            .unwrap(),
            ["zeta", "alpha", "middle"]
        );
        assert!(ordered_ids(&inventory, "zeta\nzeta\n").is_err());
        assert!(ordered_ids(&inventory, "../escape\n").is_err());
    }
}
