//! PID1 hooks inside upstream Magiskinit, before first-stage mounts.
use egysk_runtime::{context::*, fsutil as fsu, scripts, storage};
use base::libc;
use std::path::Path;
use std::time::{Duration, Instant};

pub fn early() {
    if let Err(error) = early_impl() {
        egysk_runtime::fatal_boot(&format!("Magiskinit early modules: {error:#}"));
    }
}

fn early_impl() -> Result<(), Box<dyn std::error::Error>> {
    let before = storage::mount_snapshot()?;
    if !fsu::mounted(Path::new("/dev"))? {
        fsu::mount("egysk", Path::new("/dev"), "tmpfs", libc::MS_NOSUID, "mode=0755")?;
    }
    for (name, minor, mode) in [("null", 3, 0o666), ("kmsg", 11, 0o600), ("random", 8, 0o666), ("urandom", 9, 0o666)] {
        let path = format!("/dev/{name}");
        if !Path::new(&path).exists() {
            let path = std::ffi::CString::new(path)?;
            if unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | mode, libc::makedev(1, minor)) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    let loader = Path::new("/egysk/modules/lkm-loader");
    if !loader.join("module.prop").is_file() || !loader.join("pid1.sh").is_file() {
        return Err("missing boot-payload lkm-loader module".into());
    }
    let status = std::process::Command::new("/egysk/bin/busybox")
        .args(["sh", "/egysk/modules/lkm-loader/pid1.sh"])
        .env("ASH_STANDALONE", "1").env("MODDIR", loader)
        .env("PATH", "/egysk/bin:/system/bin:/vendor/bin").status()?;
    if !status.success() { return Err(format!("lkm-loader pid1.sh: {status}").into()); }
    let (storage, modules) = storage::open_early()?;
    let deadline = Instant::now() + Duration::from_secs(35);
    let mut phases = vec!["rdinit".to_owned()];
    if let Some(extra) = storage.config.phases.get("rdinit") {
        phases.extend(extra.iter().map(|phase| format!("rdinit.{phase}")));
    }
    for phase in phases {
        for module in &modules {
            scripts::run_one(module, &Path::new(MODULES).join(&module.id), &phase,
                storage.mode, &storage.config.backing.mount_at, deadline)?;
        }
    }
    storage::save(&storage, &modules)?;
    storage::close_early(&before)?;
    // Use upstream's ordinary backup/restore and second-stage interception. The
    // backup comes from this boot's /init, never an archive's stale firmware init.
    std::fs::create_dir_all("/.egysk-backup")?;
    // RECOVERYMODE is upstream's recovery-as-boot installation flag, not the
    // current Android mode. Our bootloader already selected the right ramdisk.
    std::fs::write("/.egysk-backup/.magisk", "RECOVERYMODE=false\n")?;
    if std::fs::symlink_metadata("/init")?.file_type().is_symlink() {
        // Moving a relative link into the backup directory changes its target.
        let original = std::fs::canonicalize("/init")?;
        std::os::unix::fs::symlink(original, "/.egysk-backup/init")?;
        std::fs::remove_file("/init")?;
    } else {
        std::fs::rename("/init", "/.egysk-backup/init")?;
    }
    std::fs::copy("/proc/self/exe", "/init")?;
    Ok(())
}

/// Emitted by native patch_rc_scripts before stock init.rc. Android's early-init
/// retains ueventd, coldboot and bootstrap APEX without a blocking module action.
pub fn actions(tmp: &str) -> String {
    // Keep shell variables out of exec arguments: init expands properties both
    // when dispatching the action and when executing the temporary service.
    let guard = format!(
        r#"exec {} 0 0 -- {tmp}/bin/busybox sh -c "{tmp}/bin/egyskd"#,
        crate::consts::MAGISK_PROC_CON,
    );
    let failure = r#" || { echo 'egysk native callback failed' > /dev/kmsg; echo c > /proc/sysrq-trigger; kill -ABRT 1; exit 1; }""#;
    let adb = if egysk_runtime::debug_boot(
        &std::fs::read_to_string("/proc/bootconfig").unwrap_or_default(),
        &std::fs::read_to_string("/proc/cmdline").unwrap_or_default(),
    ) {
        let setup = format!(
            r#"exec {} 0 0 -- {tmp}/bin/busybox sh -c "{tmp}/bin/busybox sh {tmp}/bin/egysk-adb.sh{failure}"#,
            crate::consts::MAGISK_PROC_CON,
        );
        format!(r#"
on init
    {setup}
on property:apexd.status=ready
    {setup}
on post-fs-data
    {setup}
on property:sys.usb.config=*
    {setup}
"#)
    } else {
        String::new()
    };
    format!(r#"
{adb}
on init
    {guard} --egysk-stage init{failure}
on early-fs
    {guard} --egysk-stage early-fs{failure}
on post-fs
    {guard} --egysk-stage post-fs{failure}
on post-fs-data
    {guard} --post-fs-data{failure}
on property:vold.decrypt=trigger_restart_framework
    {guard} --service{failure}
on nonencrypted
    {guard} --service{failure}
on property:sys.boot_completed=1
    {guard} --boot-complete{failure}
"#)
}

pub fn label_files() {
    use base::{cstr, pre_order_walk, WalkResult};
    let root = cstr!("/dev/egysk");
    let result = (|| -> base::LoggedResult<()> {
        root.set_secontext(cstr!("u:object_r:egysk_file:s0"))?;
        pre_order_walk(root, |_, path| {
            path.set_secontext(cstr!("u:object_r:egysk_file:s0"))?;
            Ok(WalkResult::Continue)
        })?;
        cstr!("/dev/esp").set_secontext(cstr!("u:object_r:egysk_file:s0"))?;
        Ok(())
    })();
    if result.is_err() {
        egysk_runtime::fatal_boot("native runtime labeling failed");
    }
}
