//! Deterministic coverage for the opt-in read-only overlay projection planner.
//!
//! The planner runs on every boot before any projection mount, so these tests
//! fix the decisions that must not drift: module order, partition extraction,
//! replacement, whiteouts, symlinks, label sources, inherited mount flags, and
//! every rejected input.

use egysk_runtime::projection::{
    Flag, Module, Mount, OVERLAY_MARKER, Plan, SECONDARY_PARTITIONS, admission, first_conflict,
    flags_for, plan,
};
use std::ffi::CString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn marker() -> &'static str {
    OVERLAY_MARKER
}

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn real(&self) -> PathBuf {
        self.root.path().join("real")
    }

    fn staging(&self) -> PathBuf {
        self.root.path().join("staging")
    }

    fn view(&self, id: &str) -> PathBuf {
        self.root.path().join("views").join(id).join("system")
    }

    fn module(&self, id: &str) -> Module {
        Module::new(id, self.view(id))
    }

    fn real_path(&self, relative: &str) -> PathBuf {
        self.real().join(relative)
    }

    fn place_real_dir(&self, relative: &str) -> PathBuf {
        let path = self.real_path(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn place_real_file(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.real_path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn place_module_file(&self, id: &str, relative: &str, contents: &str) -> PathBuf {
        let path = self.view(id).join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn place_module_dir(&self, id: &str, relative: &str) -> PathBuf {
        let path = self.view(id).join(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn plan(&self, modules: &[Module]) -> anyhow::Result<Plan> {
        plan(
            &self.real(),
            &self.staging(),
            &SECONDARY_PARTITIONS,
            modules,
        )
    }

    fn staged(&self, relative: &str) -> PathBuf {
        self.staging().join(relative)
    }
}

fn op_to<'a>(plan: &'a Plan, path: &Path) -> &'a egysk_runtime::projection::Op {
    plan.ops
        .iter()
        .find(|op| op.to() == path)
        .unwrap_or_else(|| panic!("no operation for {}", path.display()))
}

/// Create the character device 0/0 that Magisk modules use as a whiteout.
///
/// Creating a device node needs `CAP_MKNOD`. A test environment without it
/// cannot build this fixture, so the caller skips instead of failing.
fn whiteout_at(path: &Path) -> bool {
    let path = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: the path is a valid C string and no memory is retained.
    let result = unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | 0o600, libc::makedev(0, 0)) };
    result == 0
}

fn fifo_at(path: &Path) {
    let path = CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: the path is a valid C string and no memory is retained.
    let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo failed");
}

#[test]
fn marker_name_is_the_module_opt_in() {
    assert_eq!(marker(), "overlay_readonly");
}

#[test]
fn merge_order_keeps_the_first_provider() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/etc/hosts", "real");
    fixture.place_real_dir("system/etc");
    let first = fixture.place_module_file("first", "etc/hosts", "first");
    fixture.place_module_file("first", "etc/only-first", "first");
    fixture.place_module_file("second", "etc/hosts", "second");
    fixture.place_module_file("second", "etc/only-second", "second");

    let modules = [fixture.module("first"), fixture.module("second")];
    let plan = fixture.plan(&modules).unwrap();

    let hosts = op_to(&plan, &fixture.staged("system/etc/hosts"));
    match hosts {
        egysk_runtime::projection::Op::Copy { from, attr, .. } => {
            assert_eq!(from, &first);
            assert_eq!(attr, &fixture.real_path("system/etc/hosts"));
        }
        other => panic!("unexpected operation: {other:?}"),
    }
    let targets = plan.targets();
    assert!(targets.contains(&"/system/etc/only-first".to_string()));
    assert!(targets.contains(&"/system/etc/only-second".to_string()));
    assert_eq!(
        targets,
        vec![
            "/system/etc/hosts".to_string(),
            "/system/etc/only-first".to_string(),
            "/system/etc/only-second".to_string(),
        ]
    );
}

#[test]
fn replaced_directory_hides_unprovided_real_children() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/app/Foo/real.apk", "real");
    fixture.place_real_dir("system/app/Foo/lib");
    fixture.place_module_file("debloat", "app/Foo/.replace", "");
    let module_apk = fixture.place_module_file("debloat", "app/Foo/module.apk", "module");

    let modules = [fixture.module("debloat")];
    let plan = fixture.plan(&modules).unwrap();

    assert!(matches!(
        op_to(&plan, &fixture.staged("system/app/Foo/real.apk")),
        egysk_runtime::projection::Op::Whiteout { .. }
    ));
    assert!(matches!(
        op_to(&plan, &fixture.staged("system/app/Foo/lib")),
        egysk_runtime::projection::Op::Whiteout { .. }
    ));
    match op_to(&plan, &fixture.staged("system/app/Foo/module.apk")) {
        egysk_runtime::projection::Op::Copy { from, .. } => assert_eq!(from, &module_apk),
        other => panic!("unexpected operation: {other:?}"),
    }
    // The marker itself is a decision, never staged content.
    assert!(plan.ops.iter().all(|op| !op.to().ends_with(".replace")));
    assert_eq!(
        plan.targets(),
        vec![
            "/system/app/Foo/lib".to_string(),
            "/system/app/Foo/module.apk".to_string(),
            "/system/app/Foo/real.apk".to_string(),
        ]
    );
}

#[test]
fn merged_directory_does_not_hide_real_children() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/etc/hosts", "real");
    fixture.place_module_file("module", "etc/only", "module");

    let modules = [fixture.module("module")];
    let plan = fixture.plan(&modules).unwrap();

    assert!(!plan.ops.iter().any(
        |op| matches!(op, egysk_runtime::projection::Op::Whiteout { to } if to.ends_with("hosts"))
    ));
    assert_eq!(plan.targets(), vec!["/system/etc/only".to_string()]);
}

#[test]
fn partition_extraction_follows_the_real_layout() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/bin/tool", "real");
    fixture.place_real_file("vendor/etc/conf", "real");
    fixture.place_module_file("module", "bin/tool", "module");
    fixture.place_module_file("module", "vendor/etc/conf", "module");

    let modules = [fixture.module("module")];
    let plan = fixture.plan(&modules).unwrap();

    assert_eq!(
        plan.mounts,
        vec![
            Mount {
                target: "/system".to_string(),
                staged: fixture.staged("system"),
                real: fixture.real_path("system"),
            },
            Mount {
                target: "/vendor".to_string(),
                staged: fixture.staged("vendor"),
                real: fixture.real_path("vendor"),
            },
        ]
    );
    assert_eq!(
        plan.targets(),
        vec![
            "/system/bin/tool".to_string(),
            "/vendor/etc/conf".to_string()
        ]
    );
}

#[test]
fn unbacked_partition_stays_inside_the_system_tree() {
    let fixture = Fixture::new();
    // Modern Android mounts /vendor. When the module keeps its content under
    // `system/vendor`, the real `/system/vendor` directory must exist, as the
    // native root projection requires too.
    fixture.place_real_dir("system/vendor/etc");
    let source = fixture.place_module_file("module", "vendor/etc/conf", "module");

    let modules = [fixture.module("module")];
    let plan = fixture.plan(&modules).unwrap();

    assert_eq!(plan.mounts.len(), 1);
    assert_eq!(plan.mounts[0].target, "/system");
    match op_to(&plan, &fixture.staged("system/vendor/etc/conf")) {
        egysk_runtime::projection::Op::Copy { from, .. } => assert_eq!(from, &source),
        other => panic!("unexpected operation: {other:?}"),
    }
    assert_eq!(plan.targets(), vec!["/system/vendor/etc/conf".to_string()]);
}

#[test]
fn root_addition_is_rejected_before_any_staging() {
    let fixture = Fixture::new();
    fixture.place_real_dir("system/etc");
    fixture.place_module_file("module", "bin/tool", "module");

    let modules = [fixture.module("module")];
    let error = fixture.plan(&modules).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("Unsupported root projection"),
        "unexpected error: {message}"
    );
    assert!(!fixture.staging().exists());
}

#[test]
fn partition_tree_root_replacement_is_rejected() {
    let fixture = Fixture::new();
    fixture.place_real_dir("vendor/etc");
    fixture.place_module_file("module", "vendor/.replace", "");

    let modules = [fixture.module("module")];
    let message = format!("{:#}", fixture.plan(&modules).unwrap_err());
    assert!(
        message.contains("Unsupported root projection: /vendor"),
        "unexpected error: {message}"
    );
    assert!(!fixture.staging().exists());
}

#[test]
fn partition_root_replacement_is_rejected() {
    let fixture = Fixture::new();
    fixture.place_real_dir("system/etc");
    fixture.place_module_file("module", ".replace", "");

    let modules = [fixture.module("module")];
    let message = format!("{:#}", fixture.plan(&modules).unwrap_err());
    assert!(
        message.contains("Unsupported root projection: /system"),
        "unexpected error: {message}"
    );
    assert!(!fixture.staging().exists());
}

#[test]
fn type_conflicts_are_rejected_before_any_staging() {
    let fixture = Fixture::new();
    fixture.place_real_dir("system/etc/thing");
    fixture.place_module_file("file-over-dir", "etc/thing", "module");
    let modules = [fixture.module("file-over-dir")];
    let message = format!("{:#}", fixture.plan(&modules).unwrap_err());
    assert!(
        message.contains("module file over directory"),
        "unexpected error: {message}"
    );

    fixture.place_real_file("system/etc/other", "real");
    fixture.place_module_dir("dir-over-file", "etc/other");
    let modules = [fixture.module("dir-over-file")];
    let message = format!("{:#}", fixture.plan(&modules).unwrap_err());
    assert!(
        message.contains("module directory over non-directory"),
        "unexpected error: {message}"
    );
    assert!(!fixture.staging().exists());
}

#[test]
fn whiteout_and_symlink_are_staged() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/bin/tool", "real");
    fixture.place_real_file("system/bin/link", "real");
    fs::create_dir_all(fixture.view("module").join("bin")).unwrap();
    if !whiteout_at(&fixture.view("module").join("bin/tool")) {
        eprintln!("skipping whiteout coverage: mknod requires CAP_MKNOD");
        return;
    }
    let link = fixture.view("module").join("bin/link");
    symlink("tool", &link).unwrap();

    let modules = [fixture.module("module")];
    let plan = fixture.plan(&modules).unwrap();

    assert!(matches!(
        op_to(&plan, &fixture.staged("system/bin/tool")),
        egysk_runtime::projection::Op::Whiteout { .. }
    ));
    match op_to(&plan, &fixture.staged("system/bin/link")) {
        egysk_runtime::projection::Op::Link { target, attr, .. } => {
            assert_eq!(target, "tool");
            assert_eq!(attr, &Some(fixture.real_path("system/bin/link")));
        }
        other => panic!("unexpected operation: {other:?}"),
    }
    let mut expected = vec![
        "/system/bin/link".to_string(),
        "/system/bin/tool".to_string(),
    ];
    expected.sort();
    assert_eq!(plan.targets(), expected);
}

#[test]
fn unsupported_module_nodes_are_rejected() {
    let fixture = Fixture::new();
    fixture.place_real_dir("system/bin");
    fs::create_dir_all(fixture.view("module").join("bin")).unwrap();
    fifo_at(&fixture.view("module").join("bin/fifo"));

    let modules = [fixture.module("module")];
    let message = format!("{:#}", fixture.plan(&modules).unwrap_err());
    assert!(
        message.contains("unsupported module node type"),
        "unexpected error: {message}"
    );
}

#[test]
fn label_source_prefers_the_real_target() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/etc/existing", "real");
    fixture.place_real_dir("system/etc");
    let existing = fixture.place_module_file("module", "etc/existing", "module");
    let added = fixture.place_module_file("module", "etc/added", "module");
    fixture.place_module_dir("module", "etc/newdir");

    let modules = [fixture.module("module")];
    let plan = fixture.plan(&modules).unwrap();

    match op_to(&plan, &fixture.staged("system/etc/existing")) {
        egysk_runtime::projection::Op::Copy { attr, .. } => {
            assert_eq!(attr, &fixture.real_path("system/etc/existing"))
        }
        other => panic!("unexpected operation: {other:?}"),
    }
    match op_to(&plan, &fixture.staged("system/etc/added")) {
        egysk_runtime::projection::Op::Copy { attr, .. } => assert_eq!(attr, &added),
        other => panic!("unexpected operation: {other:?}"),
    }
    match op_to(&plan, &fixture.staged("system/etc/newdir")) {
        // No real counterpart: the nearest real ancestor supplies the label.
        egysk_runtime::projection::Op::Dir { attr, .. } => {
            assert_eq!(attr, &fixture.real_path("system/etc"))
        }
        other => panic!("unexpected operation: {other:?}"),
    }
    assert_ne!(existing, added);
}

#[test]
fn partition_root_layer_copies_the_real_root_metadata() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/etc/hosts", "real");
    fixture.place_module_file("module", "etc/hosts", "module");

    let modules = [fixture.module("module")];
    let plan = fixture.plan(&modules).unwrap();

    match op_to(&plan, &fixture.staged("system")) {
        egysk_runtime::projection::Op::Dir { attr, .. } => {
            assert_eq!(attr, &fixture.real_path("system"))
        }
        other => panic!("unexpected operation: {other:?}"),
    }
}

fn mount_record(target: &str, options: &str) -> String {
    format!("42 1 0:42 / {target} {options} - ext4 /dev/block/dm-1 {options}\n")
}

#[test]
fn admission_resolves_the_deepest_covering_mount() {
    let fixture = Fixture::new();
    let real = fixture.real_path("system");
    fs::create_dir_all(&real).unwrap();
    let canonical = fs::canonicalize(&real).unwrap();
    let canonical = canonical.to_str().unwrap();

    // The root record is writable. Only the deeper covering record decides, so
    // `/system` on a writable root does not inherit the root's options.
    let info = format!(
        "{}{}",
        mount_record("/", "rw"),
        mount_record(canonical, "ro,nosuid,nodev,noexec,nosymfollow")
    );
    assert_eq!(
        admission(&info, &real).unwrap(),
        vec![
            Flag::ReadOnly,
            Flag::NoSuid,
            Flag::NoDev,
            Flag::NoExec,
            Flag::NoSymFollow,
        ]
    );

    // A path with no mount of its own uses the nearest ancestor record.
    let ancestor = mount_record("/", "ro,nodev");
    assert_eq!(
        admission(&ancestor, &real).unwrap(),
        vec![Flag::ReadOnly, Flag::NoDev]
    );
}

#[test]
fn admission_rejects_a_writable_covering_mount() {
    let fixture = Fixture::new();
    let real = fixture.real_path("system");
    fs::create_dir_all(&real).unwrap();
    // Recovery's ramdisk `/system` and an early `/system` directory on a
    // writable root have no read-only partition mount of their own.
    let message = format!(
        "{:#}",
        admission(&mount_record("/", "rw"), &real).unwrap_err()
    );
    assert!(
        message.contains("writable mount"),
        "unexpected error: {message}"
    );
}

#[test]
fn admission_requires_a_covering_mount() {
    let fixture = Fixture::new();
    let real = fixture.real_path("system");
    fs::create_dir_all(&real).unwrap();
    let message = format!("{:#}", admission("", &real).unwrap_err());
    assert!(
        message.contains("no covering mount"),
        "unexpected error: {message}"
    );
}

#[test]
fn admission_does_not_match_a_name_prefix() {
    let fixture = Fixture::new();
    let real = fixture.real_path("system");
    fs::create_dir_all(&real).unwrap();
    let canonical = fs::canonicalize(&real).unwrap();
    let info = mount_record(&format!("{}_ext", canonical.to_str().unwrap()), "ro");
    let message = format!("{:#}", admission(&info, &real).unwrap_err());
    assert!(
        message.contains("no covering mount"),
        "unexpected error: {message}"
    );
}

#[test]
fn flags_inherit_the_partition_restrictions() {
    assert_eq!(flags_for("rw,seclabel"), vec![Flag::ReadOnly]);
    assert_eq!(
        flags_for("ro,seclabel,nosuid,nodev"),
        vec![Flag::ReadOnly, Flag::NoSuid, Flag::NoDev]
    );
    assert_eq!(
        flags_for("rw,nosuid,nodev,noexec,noatime,nodiratime"),
        vec![
            Flag::ReadOnly,
            Flag::NoSuid,
            Flag::NoDev,
            Flag::NoExec,
            Flag::NoAtime,
            Flag::NoDirAtime,
        ]
    );
    assert_eq!(flags_for("rw,gid=1000,mode=0755"), vec![Flag::ReadOnly]);
    assert_eq!(
        flags_for("ro,nosymfollow"),
        vec![Flag::ReadOnly, Flag::NoSymFollow]
    );
}

#[test]
fn mount_data_orders_staged_before_real() {
    let mount = Mount {
        target: "/system".to_string(),
        staged: PathBuf::from("/dev/egysk/.egysk/overlay/system"),
        real: PathBuf::from("/system"),
    };
    assert_eq!(
        mount.data().unwrap(),
        "lowerdir=/dev/egysk/.egysk/overlay/system:/system"
    );

    let rejected = Mount {
        target: "/system".to_string(),
        staged: PathBuf::from("/dev/egysk/over:lay/system"),
        real: PathBuf::from("/system"),
    };
    assert!(rejected.data().is_err());
    let rejected = Mount {
        target: "/system".to_string(),
        staged: PathBuf::from("/dev/egysk/overlay,system"),
        real: PathBuf::from("/system"),
    };
    assert!(rejected.data().is_err());
}

#[test]
fn overlay_and_magic_paths_must_not_overlap() {
    let magic = vec![
        "/system/etc/hosts".to_string(),
        "/system/bin/tool".to_string(),
    ];
    let overlay = vec![
        "/system/bin/tool".to_string(),
        "/system/etc/hosts".to_string(),
    ];
    assert_eq!(
        first_conflict(&magic, &overlay),
        Some("/system/bin/tool".to_string())
    );
    assert_eq!(
        first_conflict(
            &["/system/etc/hosts".to_string()],
            &["/system/etc/passwd".to_string()]
        ),
        None
    );
    // A Magic Mount whole-directory bind covers the overlay content below it.
    assert_eq!(
        first_conflict(
            &["/system/etc".to_string()],
            &["/system/etc/passwd".to_string()]
        ),
        Some("/system/etc/passwd".to_string())
    );
    // An overlay whiteout hides a Magic Mount path below the name.
    assert_eq!(
        first_conflict(
            &["/system/etc/hosts".to_string()],
            &["/system/etc".to_string()]
        ),
        Some("/system/etc".to_string())
    );
    // A name prefix without a separator is a different path.
    assert_eq!(
        first_conflict(&["/system/etc".to_string()], &["/system/extra".to_string()]),
        None
    );
}

#[test]
fn empty_module_content_projects_nothing() {
    let fixture = Fixture::new();
    fixture.place_real_dir("system");
    let modules = [fixture.module("no-system-tree")];
    let plan = fixture.plan(&modules).unwrap();
    assert!(plan.ops.is_empty());
    assert!(plan.mounts.is_empty());
    assert!(plan.targets().is_empty());
}

#[test]
fn planning_is_deterministic() {
    let fixture = Fixture::new();
    fixture.place_real_file("system/etc/hosts", "real");
    fixture.place_module_file("a", "etc/hosts", "a");
    fixture.place_module_file("b", "etc/passwd", "b");

    let modules = [fixture.module("a"), fixture.module("b")];
    let first = fixture.plan(&modules).unwrap();
    let second = fixture.plan(&modules).unwrap();
    assert_eq!(first, second);

    // Parents come before their children.
    let dir = first
        .ops
        .iter()
        .position(|op| op.to() == fixture.staged("system/etc"))
        .unwrap();
    let child = first
        .ops
        .iter()
        .position(|op| op.to() == fixture.staged("system/etc/hosts"))
        .unwrap();
    assert!(dir < child);
}
