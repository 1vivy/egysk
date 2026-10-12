//! Storage adapters linked into patched Magiskinit and Magisk's native daemon.
//! Lifecycle, inventory, init RC, policy and projection belong to Magisk.
pub mod bootlog;
pub mod bootstrap;
pub mod context;
mod esp;
pub mod fsutil;
mod gpt;
pub mod scripts;
pub mod storage;
pub mod store;

use anyhow::{Result, ensure};
use context::*;
use std::path::Path;
pub use store::{
    cancel_remove, request_remove, request_remove_all, rollback, set_enabled, stage_install,
};

pub fn parse_bootstrap_config(text: &str) -> Result<BootstrapConfig> {
    ensure!(text.len() <= 65536, "bootstrap config exceeds bound");
    let config: BootstrapConfig = toml::from_str(text)?;
    config.validate()?;
    Ok(config)
}

/// Stop a failed boot with a kernel panic, preserving the cause for crash capture.
pub fn fatal_boot(message: &str) -> ! {
    use std::io::{IoSlice, Write};
    log::error!("egysk fatal boot: {message}");
    if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = file.write_vectored(&[
            IoSlice::new(b"egysk fatal boot: "),
            IoSlice::new(message.as_bytes()),
            IoSlice::new(b"\n"),
        ]);
    }
    let _ = bootlog::write(
        &[
            b"egysk fatal boot: ",
            message.as_bytes().get(..4096).unwrap_or(message.as_bytes()),
            b"\n",
        ],
        true,
    );
    if bootlog::store_mounted() {
        let _ = fsutil::atomic(
            &Path::new(STORE).join("last-failure"),
            message.as_bytes().get(..4096).unwrap_or(message.as_bytes()),
        );
    }
    // PID 1 exiting panics even when rdinit has already unmounted /proc.
    // SAFETY: getpid has no preconditions; _exit does not return or run destructors.
    if unsafe { libc::getpid() } == 1 {
        unsafe { libc::_exit(1) }
    }
    // Native stage workers are not PID 1. Use the same kernel panic control as
    // Android's init_fatal_panic, rather than rebooting away the failure.
    let panic = std::fs::OpenOptions::new()
        .write(true)
        .open("/proc/sysrq-trigger")
        .and_then(|mut trigger| trigger.write_all(b"c"));
    if let Err(error) = panic {
        log::error!("egysk fatal boot: kernel panic request failed: {error}");
    }
    // Never continue the stage or substitute a silent reboot if panic is denied.
    loop {
        std::thread::park_timeout(std::time::Duration::from_secs(3600));
    }
}

/// Bootconfig overrides cmdline per key, including explicitly disabled flags.
fn boot_value<'a>(bootconfig: &'a str, cmdline: &'a str, key: &str) -> Option<&'a str> {
    bootconfig
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"'))
        })
        .or_else(|| {
            cmdline.split_whitespace().find_map(|word| {
                let (k, v) = word.split_once('=')?;
                (k == key).then_some(v)
            })
        })
}

pub fn debug_boot(bootconfig: &str, cmdline: &str) -> bool {
    boot_value(bootconfig, cmdline, "androidboot.egysk.debug") == Some("1")
}

/// Native policies schema: Allow = 2. Reconcile this row on every daemon start.
pub fn shell_policy_sql(debug: bool) -> &'static str {
    if debug {
        "INSERT OR REPLACE INTO policies (uid,policy,until,logging,notification) VALUES (2000,2,0,1,1)"
    } else {
        "DELETE FROM policies WHERE uid=2000"
    }
}

/// First-stage classifier ported from egyskinit; bootconfig overrides cmdline per key.
pub fn boot_mode(bootconfig: &str, cmdline: &str, recovery_present: bool) -> BootMode {
    let value = |key| boot_value(bootconfig, cmdline, key);
    if matches!(
        value("androidboot.mode"),
        Some("recovery" | "fastboot" | "fastbootd")
    ) || value("androidboot.recovery") == Some("1")
    {
        BootMode::Recovery
    } else if value("androidboot.mode") == Some("charger") {
        BootMode::Charger
    } else if recovery_present && value("androidboot.force_normal_boot") != Some("1") {
        BootMode::Recovery
    } else {
        BootMode::Normal
    }
}
