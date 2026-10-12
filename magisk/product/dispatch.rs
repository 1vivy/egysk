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
