//! ESP storage configuration and Unix view metadata, shared with offline producers.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const ROOT: &str = "/dev/egysk";
pub const ESP: &str = "/dev/esp";
pub const PACKAGE: &str = "/dev/esp/egysk";
pub const BIN: &str = "/dev/egysk/bin";
pub const STORE: &str = "/dev/egysk/store";
pub const MODULES: &str = "/dev/egysk/modules";
pub const LOWERS: &str = "/dev/egysk/lowers";
pub const BOOTSTRAP_CONFIG: &str = "/egysk.toml";
pub const PACKAGE_CONFIG: &str = "/dev/esp/egysk/egysk.toml";
pub const INDEPENDENT_BIN: &str = "/egysk/bin";
pub const DEVICE_DIR: &str = "/dev/egysk/devices";
pub const SELECTED_ESP: &str = "/dev/egysk/devices/esp";
pub const SELECTED_BACKING: &str = "/dev/egysk/devices/backing";
pub const RD: &str = "/dev/egysk/rd";
pub const DAEMON: &str = "egyskd";
pub const POLICY_TOOL: &str = "magiskpolicy";
pub const INIT_PRELOAD: &str = "init-ld";
pub const FILE_CONTEXT: &str = "u:object_r:egysk_file:s0";
pub const BOOTSTRAP_TOOLS: [&str; 8] = [
    DAEMON,
    "busybox",
    POLICY_TOOL,
    "util_functions.sh",
    INIT_PRELOAD,
    "egysk-adb.sh",
    "egysk-adbd",
    "libegysk-adb.so",
];

// Persisted storage-format markers, not executable/product aliases. Keeping
// these bytes preserves generation identity and interrupted activation journals.
pub const GENERATION_MARKER: &str = ".esp-generation";
pub const TRANSACTION_TEMP_EXTENSION: &str = "esp-tmp";

/// Owned filesystem mount options shared by the native storage entry points.
pub struct MountSpec {
    pub fs_type: &'static str,
    pub flags: libc::c_ulong,
    pub data: &'static str,
}
pub const ESP_MOUNT: MountSpec = MountSpec {
    fs_type: "vfat",
    flags: libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
    data: "uid=0,gid=0,fmask=0022,dmask=0022",
};
pub const EXECUTABLE_MOUNT: MountSpec = MountSpec {
    fs_type: "tmpfs",
    flags: libc::MS_NOSUID | libc::MS_NODEV,
    data: "mode=0755",
};
pub const MOUNT_SOURCE: &str = "egysk";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Backing {
    /// `block` (an explicit physical block node) or `filesystem` (already mounted).
    pub kind: String,
    /// Exactly one source/partition for block; filesystem accepts source only.
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub partition: Option<String>,
    pub fs_type: String,
    pub mount_at: String,
    /// Block backing helper; filesystem backing is adopted natively after init.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// Producer-only KMI and LKM fields are intentionally not deserialized.
pub struct BootstrapConfig {
    #[serde(default)]
    pub esp_device: Option<String>,
    pub backing: Backing,
    /// Additional script barriers: stage.sh first, then stage.<phase>.sh in this order.
    #[serde(default)]
    pub phases: BTreeMap<String, Vec<String>>,
    /// Extra package bin files to copy to executable tmpfs (installer/tool inputs).
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub safe_mode: bool,
    #[serde(default)]
    pub norc: bool,
}

pub const HOOKS: &[&str] = &[
    "rdinit",
    "early-init",
    "init",
    "early-fs",
    "post-fs",
    "post-fs-data",
    "post-mount",
    "service",
    "boot-completed",
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BootMode {
    Normal,
    Recovery,
    Charger,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    Esp,
    Local,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
    pub id: String,
    pub generation: String,
    pub owner: Owner,
}

pub fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid identifier {value:?}"
    );
    Ok(())
}

pub fn absolute(value: &str) -> Result<()> {
    ensure!(
        value.starts_with('/')
            && value.len() <= 4096
            && !value
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b",:\\\0'\"$;".contains(&b))
            && Path::new(value).components().all(|c| matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )),
        "unsafe absolute path {value:?}"
    );
    Ok(())
}

impl BootstrapConfig {
    /// Resolve shared package settings against this boot's ramdisk selection.
    pub fn for_boot(mut self, bootstrap: Self) -> Result<Self> {
        ensure!(
            bootstrap.backing == self.backing,
            "cpio/package backing mismatch"
        );
        self.safe_mode |= bootstrap.safe_mode;
        self.norc |= bootstrap.norc;
        Ok(self)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.backing.kind.as_str(), "block" | "filesystem"),
            "invalid backing kind"
        );
        ensure!(
            self.backing.source.is_some() != self.backing.partition.is_some(),
            "backing requires exactly one source or partition"
        );
        if let Some(source) = &self.backing.source {
            absolute(source)?;
        }
        if let Some(partition) = &self.backing.partition {
            ensure!(
                self.backing.kind == "block",
                "filesystem backing cannot select a partition"
            );
            identifier(partition)?;
        }
        absolute(&self.backing.mount_at)?;
        identifier(&self.backing.fs_type)?;
        if self.backing.kind == "filesystem" {
            ensure!(
                self.backing.source.as_deref() == Some(self.backing.mount_at.as_str()),
                "filesystem backing must adopt its source mount"
            );
            ensure!(
                self.backing.helper.is_none() && self.backing.args.is_empty(),
                "filesystem backing does not use a helper"
            );
        } else {
            identifier(
                self.backing
                    .helper
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("block backing requires a helper"))?,
            )?;
        }
        if let Some(path) = &self.esp_device {
            absolute(path)?;
        }
        for name in &self.tools {
            identifier(name)?;
        }
        for (stage, phases) in &self.phases {
            ensure!(
                HOOKS.contains(&stage.as_str()),
                "unknown phase stage {stage}"
            );
            let mut seen = BTreeSet::new();
            for phase in phases {
                identifier(phase)?;
                ensure!(seen.insert(phase), "duplicate phase {phase}");
            }
        }
        Ok(())
    }
}
