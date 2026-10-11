//! Vendor load-list boot mode selection; no core-runtime dependency.
#[derive(Clone, Copy)]
pub(crate) enum BootMode {
    Normal,
    Recovery,
    Charger,
}
pub(crate) const RECOVERY_EXECUTABLE: &str = "/system/bin/recovery";
pub(crate) fn classify_boot_mode(
    bootconfig: &str,
    cmdline: &str,
    recovery_present: bool,
) -> BootMode {
    let value = |key: &str| {
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
    };
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
