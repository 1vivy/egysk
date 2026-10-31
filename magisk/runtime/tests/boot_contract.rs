use egysk_runtime::{boot_mode, context::*, fsutil, parse_bootstrap_config, store};
use std::fs;
use std::path::Path;

fn config() -> BootstrapConfig {
    parse_bootstrap_config(
        r#"
kmi = "android16-6.12-202509"
tools = ["maos-platform"]
[backing]
kind = "block"
source = "/dev/block/by-name/metadata"
fs_type = "f2fs"
mount_at = "/dev/egysk/metadata"
helper = "maos-credentials"
[phases]
rdinit = ["identity", "storage", "provide", "avb", "projection"]
"#,
    )
    .unwrap()
}

#[test]
fn debug_boot_requires_exact_one_and_bootconfig_takes_precedence() {
    use egysk_runtime::debug_boot;
    assert!(!debug_boot("", ""));
    assert!(debug_boot("", "androidboot.egysk.debug=1"));
    assert!(debug_boot("androidboot.egysk.debug = \"1\"", ""));
    for value in ["", "0", "true", "01", "2", "1x"] {
        let cmdline = format!("androidboot.egysk.debug={value}");
        let bootconfig = format!("androidboot.egysk.debug = \"{value}\"");
        assert!(!debug_boot("", &cmdline));
        assert!(!debug_boot(&bootconfig, "androidboot.egysk.debug=1"));
    }
    assert!(debug_boot(
        "androidboot.egysk.debug = \"1\"",
        "androidboot.egysk.debug=0"
    ));
}

#[test]
fn product_boot_removes_prior_shell_grant_without_changing_other_policies() {
    // Python is already required by the native producer. Exercise real SQLite
    // with the same SQL passed to Magisk's own db layer, not a SQL wording test.
    let result = std::process::Command::new("python3")
        .args([
            "-c",
            r#"
import sqlite3, sys
db = sqlite3.connect(":memory:")
db.execute("CREATE TABLE policies (uid INT PRIMARY KEY, policy INT, until INT, logging INT, notification INT)")
db.execute("INSERT INTO policies VALUES (12345,1,0,0,0)")
for _ in range(2):
    db.execute(sys.argv[1])
    assert db.execute("SELECT policy,until FROM policies WHERE uid=2000").fetchall() == [(2,0)]
for _ in range(2):
    db.execute(sys.argv[2])
    assert db.execute("SELECT * FROM policies WHERE uid=2000").fetchall() == []
assert db.execute("SELECT * FROM policies").fetchall() == [(12345,1,0,0,0)]
"#,
            egysk_runtime::shell_policy_sql(true),
            egysk_runtime::shell_policy_sql(false),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn producer_only_kernel_selection_is_not_a_runtime_contract() {
    let text = r#"
        kmi = "producer-only"
        critical_modules = [{ name = "loader-owned", path = "/module.ko" }]
        [backing]
        kind = "filesystem"
        source = "/metadata"
        fs_type = "f2fs"
        mount_at = "/metadata"
    "#;
    let config = parse_bootstrap_config(text).unwrap();
    let saved = serde_json::to_string(&config).unwrap();
    assert!(!saved.contains("kmi"));
    assert!(!saved.contains("critical_modules"));
}

#[test]
fn a_seed_selects_the_store_volume_through_the_helper_selector() {
    let text = r#"
        kmi = "android16-6.12-202509"
        [backing]
        kind = "block"
        fs_type = "ext4"
        mount_at = "/dev/egysk/metadata"
        helper = "maos-credentials"
        args = ["--lv", "rom/store"]
    "#;
    let seed = parse_bootstrap_config(text).unwrap();
    assert_eq!(seed.backing.args, ["--lv", "rom/store"]);
    assert!(seed.backing.source.is_none());
    assert!(seed.backing.partition.is_none());
    assert!(seed.clone().for_boot(seed.clone()).is_ok());

    // Exactly one selector: the arguments replace source and partition.
    let mut both = seed.clone();
    both.backing.source = Some("/dev/egysk/devices/backing".into());
    assert!(both.validate().is_err());
    let mut both = seed.clone();
    both.backing.partition = Some("store".into());
    assert!(both.validate().is_err());
    let mut neither = seed.clone();
    neither.backing.args.clear();
    assert!(neither.validate().is_err());
    let mut empty = seed;
    empty.backing.args.push(String::new());
    assert!(empty.validate().is_err());
}

#[test]
fn shared_package_cannot_redirect_the_bootstrap_backing() {
    let mut package = config();
    package.backing.source = Some("/dev/block/by-name/other-metadata".into());
    assert!(package.for_boot(config()).is_err());
}

#[test]
fn direct_backing_adopts_stock_mount_without_helper_and_must_match_package() {
    let text = r#"
        [backing]
        kind = "filesystem"
        source = "/metadata"
        fs_type = "f2fs"
        mount_at = "/metadata"
    "#;
    let direct = parse_bootstrap_config(text).unwrap();
    assert!(direct.backing.helper.is_none());
    assert!(direct.clone().for_boot(direct.clone()).is_ok());
    let mut changed = direct.clone();
    changed.backing.fs_type = "ext4".into();
    assert!(changed.for_boot(direct.clone()).is_err());
    let mut changed = direct.clone();
    changed.backing.mount_at = "/dev/egysk/metadata".into();
    assert!(changed.validate().is_err());
    let mut changed = direct;
    changed.backing.helper = Some("maos-credentials".into());
    assert!(changed.validate().is_err());
    assert!(parse_bootstrap_config(&text.replace("filesystem", "block")).is_err());
}

#[test]
fn property_flags_and_conventional_magisk_markers_are_supported() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("module.prop"),
        "id=test\ncritical=true\nskip_mount=1\n",
    )
    .unwrap();
    store::identify(root.path(), "test").unwrap();
    assert!(store::valued_flag(root.path(), "critical").unwrap());
    assert!(store::valued_flag(root.path(), "skip_mount").unwrap());
    for value in ["", "false", "0", "TRUE", "yes"] {
        fs::write(root.path().join("critical"), value).unwrap();
        assert!(!store::valued_flag(root.path(), "critical").unwrap());
    }
    for value in ["1", "true", "true\n"] {
        fs::write(root.path().join("critical"), value).unwrap();
        assert!(store::valued_flag(root.path(), "critical").unwrap());
    }
    for marker in ["disable", "remove", "skip_mount"] {
        fs::write(root.path().join(marker), "").unwrap();
        assert!(store::valued_flag(root.path(), marker).unwrap());
        fs::write(root.path().join(marker), "0").unwrap();
        assert!(!store::valued_flag(root.path(), marker).unwrap());
    }
    assert!(store::identify(root.path(), "other").is_err());
    fs::write(root.path().join("module.prop"), "id=test\nid=test\n").unwrap();
    assert!(store::identify(root.path(), "test").is_err());
}

#[test]
fn virtual_only_modules_are_admitted_only_for_the_virtual_owner() {
    let root = tempfile::tempdir().unwrap();
    // A module without the flag is admitted in every boot-control mode.
    for owner in [None, Some("physical"), Some("virtual"), Some("other")] {
        assert!(!store::virtual_only_skipped_for(root.path(), owner).unwrap());
    }
    fs::write(root.path().join("virtual_only"), "1\n").unwrap();
    assert!(!store::virtual_only_skipped_for(root.path(), Some("virtual")).unwrap());
    // Physical, unknown and absent owners never admit the module: its RC, its
    // sepolicy.rule and its stage scripts stay out of the boot.
    for owner in [None, Some("physical"), Some("other"), Some("Virtual")] {
        assert!(store::virtual_only_skipped_for(root.path(), owner).unwrap());
    }
}

#[test]
fn early_identity_provider_is_a_named_package_tool() {
    let backing = r#"
        [backing]
        kind = "filesystem"
        source = "/metadata"
        fs_type = "f2fs"
        mount_at = "/metadata"
    "#;
    assert!(
        parse_bootstrap_config(backing)
            .unwrap()
            .early_identity
            .is_none()
    );
    let named = format!("{backing}[early_identity]\ntool = \"maos-platform\"\n");
    let provider = parse_bootstrap_config(&named).unwrap().early_identity;
    assert_eq!(provider.unwrap().tool, "maos-platform");
    let empty = format!("{backing}[early_identity]\n");
    assert!(parse_bootstrap_config(&empty).is_err());
    let unsafe_tool = format!("{backing}[early_identity]\ntool = \"../maos\"\n");
    assert!(parse_bootstrap_config(&unsafe_tool).is_err());
}

#[test]
fn generation_is_content_sensitive_and_safe_links_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("system")).unwrap();
    fs::write(root.path().join("system/file"), "old").unwrap();
    std::os::unix::fs::symlink("file", root.path().join("system/link")).unwrap();
    let old = fsutil::generation(root.path()).unwrap();
    fs::write(root.path().join("system/file"), "new").unwrap();
    let new = fsutil::generation(root.path()).unwrap();
    assert_ne!(old, new);
    fs::write(root.path().join(".esp-generation"), &new).unwrap();
    assert_eq!(fsutil::generation(root.path()).unwrap(), new);
    let destination = tempfile::tempdir().unwrap();
    let target = destination.path().join("copy");
    fsutil::copy_tree(root.path(), &target).unwrap();
    assert_eq!(
        fs::read_link(target.join("system/link")).unwrap(),
        Path::new("file")
    );
    std::os::unix::fs::symlink("../../escape", root.path().join("system/bad")).unwrap();
    assert!(fsutil::generation(root.path()).is_err());
}

#[test]
fn boot_mode_and_storage_config_remain_userspace_owned() {
    assert_eq!(
        boot_mode(
            "androidboot.mode = \"recovery\"",
            "androidboot.mode=charger",
            false
        ),
        BootMode::Recovery
    );
    assert_eq!(
        boot_mode("", "androidboot.mode=charger", false),
        BootMode::Charger
    );
    assert_eq!(
        boot_mode("androidboot.force_normal_boot = \"1\"", "", true),
        BootMode::Normal
    );
    assert_eq!(boot_mode("", "", true), BootMode::Recovery);
    let mut value = config();
    value.backing.source = Some("/dev/../metadata".into());
    assert!(value.validate().is_err());
    value = config();
    value.backing.partition = Some("metadata".into());
    assert!(value.validate().is_err());
    value.backing.source = None;
    assert!(value.validate().is_ok());
    value.backing.kind = "filesystem".into();
    assert!(value.validate().is_err());
}

#[test]
fn foreground_deadline_kills_and_reaps_the_owned_process_group() {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "sleep 30"]).process_group(0);
    egysk_runtime::scripts::child_signals(&mut command);
    let mut child = command.spawn().unwrap();
    let pid = child.id() as i32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    assert!(egysk_runtime::scripts::wait_until(&mut child, deadline, true).is_err());
    assert_eq!(
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn foreground_completion_observes_script_failure_before_consumers() {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "sleep 0.05; exit 23"]).process_group(0);
    let mut child = command.spawn().unwrap();
    let result = egysk_runtime::scripts::wait_until(
        &mut child,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        false,
    );
    assert!(result.is_err());
    assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(23));
}

#[test]
fn foreground_stderr_cannot_fill_the_pipe_and_hide_the_failure_tail() {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args([
            "-c",
            "set -e; dd if=/dev/zero bs=4096 count=32 >&2 2>/dev/null; printf 'final failure cause' >&2; exit 23",
        ])
        .stderr(std::process::Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let error = egysk_runtime::scripts::wait_until(
        &mut child,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        true,
    )
    .unwrap_err();
    assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(23));
    let detail = error.to_string();
    assert!(detail.ends_with("final failure cause"));
    assert!(detail.len() < 1024);
}
