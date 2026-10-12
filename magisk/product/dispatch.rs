//! Additional filenames executed by Magisk's native script runner and inventory.
use crate::ffi::{ModuleInfo, exec_module_scripts};
use base::Utf8CString;
use std::sync::LazyLock;

// Immutable filesystem inputs, not a second inventory or lifecycle state.
static STORAGE: LazyLock<egysk_runtime::storage::Storage> = LazyLock::new(|| {
    egysk_runtime::storage::read().unwrap_or_else(|error| {
        egysk_runtime::fatal_boot(&format!("module hook storage: {error:#}"))
    })
});

pub fn is_recovery() -> bool {
    STORAGE.mode == egysk_runtime::context::BootMode::Recovery
}

pub fn debug_boot() -> bool {
    STORAGE.debug
}

/// Reconcile before opening the daemon socket: a prior dev boot cannot leak its grant.
pub fn configure_shell_policy(daemon: &crate::daemon::MagiskD) {
    const _: () = assert!(crate::ffi::SuPolicy::Allow.repr == 2);
    if daemon.db_exec(egysk_runtime::shell_policy_sql(debug_boot()), &[]) != 0 {
        egysk_runtime::fatal_boot("cannot reconcile boot-scoped shell su policy");
    }
}

pub fn hooks(stage: &str, modules: &Vec<ModuleInfo>) {
    let storage = &*STORAGE;
    exec_module_scripts(&Utf8CString::from(stage), modules);
    if let Some(phases) = storage.config.phases.get(stage) {
        for phase in phases {
            exec_module_scripts(&Utf8CString::from(format!("{stage}.{phase}")), modules);
        }
    }
}

/// Called only in Magisk's forked script child, never in the threaded daemon.
pub fn product_script_env() {
    let storage = &*STORAGE;
    unsafe {
        std::env::set_var("EGYSK_BACKING_ROOT", &storage.config.backing.mount_at);
        std::env::set_var("EGYSK_MODE", match storage.mode {
            egysk_runtime::context::BootMode::Normal => "normal",
            egysk_runtime::context::BootMode::Recovery => "recovery",
            egysk_runtime::context::BootMode::Charger => "charger",
        });
    }
}

/// A completed boot blesses the counted entry this boot launched (systemd
/// automatic boot assessment): the volatile `LoaderBootCountPath` variable names
/// the file Surfacer renamed right before launching it, and this point is only
/// reached once Android's `sys.boot_completed=1` ran every module hook.
///
/// Only the direct descriptor (filesystem backing adopted from stock init, with
/// no managed ROM projection) leaves blessing to egysk; a managed ROM's own
/// userspace blesses the entries it counted. Blessing is an idempotent rename of
/// that one file, so a case both visit is harmless, and a failure is logged
/// rather than fatal: the boot did succeed.
pub fn bless_boot() {
    if STORAGE.config.backing.kind != "filesystem" {
        return;
    }
    match egysk_runtime::bless::bless_loaded_entry() {
        Ok(None) => {}
        Ok(Some(path)) => base::info!("** blessed boot entry: {}", path.display()),
        Err(error) => base::error!("bless counted boot entry: {error:#}"),
    }
}
