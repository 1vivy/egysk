//! Mounts that this product creates.
//!
//! Magisk owns projection. This module only names the mounts that the
//! projection, the module views, and the product runtime add to a namespace,
//! so a cleanup can detach them from a target mount namespace. A mount belongs
//! to this product when its source is `egysk`, or when its device and root
//! identify the store or a module or state subtree in PID 1's mount namespace.
//! The selectors also require an Android export root, a path under the product
//! root, or the exact package backing mount. A product-source layer on a
//! partition root is the opt-in read-only overlay projection. A foreign mount or
//! a path that only shares a name prefix does not qualify.
//!
//! `projection_exports` returns Android export mounts. `namespace_mounts`
//! returns the export mounts and the product mounts in the target namespace.
//! Both return the deepest target first and one entry per owned layer.

use crate::context::{ESP, MOUNT_SOURCE, ROOT, SELECTED_ESP, STORE};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;

/// Android roots that the projection writes to.
const EXPORT_ROOTS: [&str; 4] = ["/system", "/vendor", "/product", "/system_ext"];

struct MountPoint<'a> {
    device: &'a str,
    root: Cow<'a, str>,
    target: Cow<'a, str>,
    vfs_option: String,
    source: Cow<'a, str>,
}

struct StoreRoots<'a> {
    device: &'a str,
    root: &'a str,
    modules: String,
    state: String,
}

/// The store identity of one namespace, valid only when exactly one mount
/// record names the store target. A stacked target proves nothing.
fn store_key<'a, 'b: 'a>(entries: &'a [MountPoint<'b>]) -> Option<StoreRoots<'a>> {
    let mut stores = entries.iter().filter(|mount| mount.target == STORE);
    let candidate = stores.next()?;
    if stores.next().is_some() {
        return None;
    }
    Some(StoreRoots {
        device: candidate.device,
        root: candidate.root.as_ref(),
        modules: join(&candidate.root, "modules"),
        state: join(&candidate.root, "state"),
    })
}

fn below(path: &str, root: &str) -> bool {
    let path = Path::new(path);
    path != Path::new(root) && path.starts_with(root)
}

fn at_or_below(path: &str, root: &str) -> bool {
    path == root || below(path, root)
}

fn join(root: &str, name: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{name}")
    } else {
        format!("{root}/{name}")
    }
}

fn unescape(value: &str) -> Cow<'_, str> {
    if !value.as_bytes().contains(&b'\\') {
        return Cow::Borrowed(value);
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let octal = index + 4 <= bytes.len()
            && bytes[index] == b'\\'
            && bytes[index + 1..index + 4]
                .iter()
                .all(|byte| matches!(*byte, b'0'..=b'7'));
        if octal {
            let digit = |byte: u8| u32::from(byte - b'0');
            let value = digit(bytes[index + 1]) * 64
                + digit(bytes[index + 2]) * 8
                + digit(bytes[index + 3]);
            out.push(u8::try_from(value).unwrap_or(b'\\'));
            index += 4;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Cow::Owned(match String::from_utf8(out) {
        Ok(decoded) => decoded,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    })
}

fn parse(mountinfo: &str) -> Vec<MountPoint<'_>> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let (mount, filesystem) = line.split_once(" - ")?;
            let mut fields = mount.split_whitespace();
            let _id = fields.next()?;
            let _parent = fields.next()?;
            let device = fields.next()?;
            let root = unescape(fields.next()?);
            let target = unescape(fields.next()?);
            let vfs_option = fields.next()?.to_string();
            let source = filesystem.split_whitespace().nth(1)?;
            Some(MountPoint {
                device,
                root,
                target,
                vfs_option,
                source: unescape(source),
            })
        })
        .collect()
}

/// A mount covering a canonical path, with the options that decide whether a
/// projection may be mounted over it.
#[derive(Debug)]
pub struct CoveringMount {
    /// Target of the covering mount record.
    pub target: String,
    /// VFS options of that record, for example `ro,nosuid,nodev`.
    pub vfs_option: String,
}

/// The deepest mount covering `target`, or the topmost layer on the same target.
///
/// `target` must be canonical: the caller resolves symbolic links first. The
/// match is by path components, so `/system_ext` does not cover `/system`. A
/// missing record stays missing: the caller rejects the projection instead of
/// falling back to default options.
pub fn covering_mount(mountinfo: &str, target: &str) -> Option<CoveringMount> {
    let mut best: Option<(usize, CoveringMount)> = None;
    for mount in parse(mountinfo) {
        if mount.target.as_ref() == target || below(target, &mount.target) {
            let depth = depth(&mount.target);
            if best
                .as_ref()
                .is_none_or(|(best_depth, _)| depth >= *best_depth)
            {
                best = Some((
                    depth,
                    CoveringMount {
                        target: mount.target.to_string(),
                        vfs_option: mount.vfs_option,
                    },
                ));
            }
        }
    }
    best.map(|(_, mount)| mount)
}

/// A mount holds module content when its source is this product, or when it
/// comes from the store filesystem below the store's module directory.
fn module_content(mount: &MountPoint<'_>, store: Option<&StoreRoots<'_>>) -> bool {
    if mount.source == MOUNT_SOURCE {
        return true;
    }
    store.is_some_and(|store| mount.device == store.device && below(&mount.root, &store.modules))
}

/// A mount belongs to the product runtime when its source is this product, or
/// when it mirrors a store module or state subtree. The store bind itself
/// counts only when the caller anchors it to a trusted record.
fn runtime_mount(mount: &MountPoint<'_>, store: Option<&StoreRoots<'_>>, store_bind: bool) -> bool {
    if mount.source == MOUNT_SOURCE {
        return true;
    }
    store.is_some_and(|store| {
        mount.device == store.device
            && (at_or_below(&mount.root, &store.modules)
                || below(&mount.root, &store.state)
                || (store_bind && mount.root == store.root))
    })
}

/// The package mount that this product created from its own device node.
fn package_mount(mount: &MountPoint<'_>) -> bool {
    mount.target == ESP && mount.source == SELECTED_ESP
}

fn export(target: &str) -> bool {
    EXPORT_ROOTS.iter().any(|root| below(target, root))
}

/// A product-source layer on a partition root itself.
///
/// The Magic Mount projection writes strictly below those roots, so an exact
/// target is the whole-partition read-only overlay layer that the opt-in
/// `overlay_readonly` projection mounts. A platform or foreign mount on the
/// same root keeps its own source and stays unowned.
fn partition_layer(mount: &MountPoint<'_>) -> bool {
    mount.source == MOUNT_SOURCE
        && EXPORT_ROOTS
            .iter()
            .any(|root| at_or_below(&mount.target, root))
}

fn selected(
    mount: &MountPoint<'_>,
    store: Option<&StoreRoots<'_>>,
    store_bind: bool,
    views: bool,
) -> bool {
    if partition_layer(mount) {
        return true;
    }
    if module_content(mount, store) && export(&mount.target) {
        return true;
    }
    if !views {
        return false;
    }
    (runtime_mount(mount, store, store_bind) && at_or_below(&mount.target, ROOT))
        || package_mount(mount)
}

fn depth(target: &str) -> usize {
    Path::new(target).components().count()
}

fn select(
    entries: &[MountPoint<'_>],
    store: Option<&StoreRoots<'_>>,
    store_bind: bool,
    views: bool,
) -> Vec<String> {
    // A target can hold several stacked mounts. The topmost layer leaves first,
    // so an unowned layer stops the walk and keeps the layers below it.
    let mut blocked: BTreeSet<&str> = BTreeSet::new();
    let mut targets = Vec::new();
    for mount in entries.iter().rev() {
        if blocked.contains(mount.target.as_ref()) {
            continue;
        }
        if selected(mount, store, store_bind, views) {
            targets.push(mount.target.to_string());
        } else {
            blocked.insert(mount.target.as_ref());
        }
    }
    // Deepest first: a directory mount leaves after the file mounts it holds.
    targets.sort_by(|a, b| depth(b).cmp(&depth(a)).then_with(|| b.cmp(a)));
    targets
}

fn collect(mountinfo: &str, parent_mountinfo: &str, views: bool) -> Vec<String> {
    let entries = parse(mountinfo);
    let parents = parse(parent_mountinfo);
    let store = store_key(&parents).filter(|store| store.root != "/");
    let store_bind = store.is_some();
    select(&entries, store.as_ref(), store_bind, views)
}

/// Android export targets that the projection created from module content.
///
/// A caller that rebuilds the projection detaches these targets before it
/// mounts the new tree. The module views stay mounted, because they are the
/// source of the new projection. The store bind is never an export target.
/// Targets are paths below an export root and, for the opt-in read-only
/// overlay projection, the product-source layer on an export root itself.
/// The store identification comes from `parent_mountinfo`; the daemon runs in
/// the PID 1 mount namespace, so that record is the daemon's own view.
pub fn projection_exports(mountinfo: &str, parent_mountinfo: &str) -> Vec<String> {
    collect(mountinfo, parent_mountinfo, false)
}

/// Every product mount target in the namespace, deepest first.
///
/// A caller that gives a target process its own view detaches these targets
/// from that namespace. Ownership is anchored to the parent namespace record:
/// the store identification comes from `parent_mountinfo`, not from the target
/// namespace. A missing or ambiguous parent record, or a target store that
/// disagrees with it, leaves only product-source mounts and the package mount.
pub fn namespace_mounts(mountinfo: &str, parent_mountinfo: &str) -> Vec<String> {
    collect(mountinfo, parent_mountinfo, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{LOWERS, MODULES};

    const STORE_DEVICE: &str = "0:166";
    const STORE_SOURCE: &str = "/dev/block/dm-9";
    const OVERLAY_DEVICE: &str = "0:170";
    const FOREIGN_DEVICE: &str = "0:100";

    fn store() -> String {
        format!("10 1 {STORE_DEVICE} /egysk {STORE} rw - ext4 {STORE_SOURCE} rw\n")
    }

    fn product(root: &str, target: &str) -> String {
        format!("20 1 {OVERLAY_DEVICE} {root} {target} rw - overlay {MOUNT_SOURCE} rw\n")
    }

    fn stored(root: &str, target: &str) -> String {
        format!("21 1 {STORE_DEVICE} {root} {target} rw - ext4 {STORE_SOURCE} rw\n")
    }

    fn mounted(device: &str, root: &str, target: &str, source: &str) -> String {
        format!("22 1 {device} {root} {target} rw - ext4 {source} rw\n")
    }

    fn package(source: &str) -> String {
        format!("30 1 0:179 / {ESP} rw - vfat {source} rw\n")
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|target| target.to_string()).collect()
    }

    /// Target namespace view when PID 1 reports the product store.
    fn anchored(info: &str) -> Vec<String> {
        namespace_mounts(info, &store())
    }

    /// Target namespace view when the parent record is unavailable.
    fn unanchored(info: &str) -> Vec<String> {
        namespace_mounts(info, "")
    }

    /// Export view anchored to the daemon's own namespace record.
    fn anchored_exports(info: &str) -> Vec<String> {
        projection_exports(info, &store())
    }

    #[test]
    fn product_source_mounts_are_owned() {
        let mut info = store();
        info.push_str(&product("/system/bin/tool", "/system/bin/tool"));
        info.push_str(&product("/", &format!("{MODULES}/foo")));
        info.push_str(&product("/", LOWERS));
        let exports = names(&["/system/bin/tool"]);
        assert_eq!(anchored_exports(&info), exports);
        let expected = names(&[&format!("{MODULES}/foo"), "/system/bin/tool", LOWERS]);
        assert_eq!(unanchored(&info), expected);
    }

    #[test]
    fn store_mounts_and_state_binds_are_owned() {
        let mut info = store();
        info.push_str(&stored(
            "/egysk/modules/foo/system/bin/tool",
            "/system/bin/tool",
        ));
        info.push_str(&stored("/egysk/modules/foo", &format!("{MODULES}/foo")));
        info.push_str(&stored("/egysk/state/foo", &format!("{ROOT}/state/foo")));
        let exports = names(&["/system/bin/tool"]);
        assert_eq!(anchored_exports(&info), exports);
        let mut expected = names(&[&format!("{ROOT}/state/foo"), &format!("{MODULES}/foo")]);
        expected.push("/system/bin/tool".to_string());
        expected.push(STORE.to_string());
        assert_eq!(anchored(&info), expected);
    }

    #[test]
    fn other_store_paths_are_not_exported() {
        let mut info = store();
        info.push_str(&stored("/egysk/config/other", "/system/bin/tool"));
        info.push_str(&stored("/egysk/preferences/foo", "/system/bin/other"));
        info.push_str(&mounted(
            STORE_DEVICE,
            "/egysk",
            "/system/bin/store",
            STORE_SOURCE,
        ));
        info.push_str(&stored("/egysk/modules_extra/foo", "/system/bin/extra"));
        assert!(anchored_exports(&info).is_empty());
        assert_eq!(anchored(&info), names(&[STORE]));
    }

    #[test]
    fn foreign_store_paths_are_not_owned() {
        let mut info = store();
        info.push_str(&stored("/Android/data/other", "/system/bin/tool"));
        assert!(anchored_exports(&info).is_empty());
        assert_eq!(anchored(&info), names(&[STORE]));
    }

    #[test]
    fn platform_and_foreign_root_mounts_are_not_owned() {
        let mut info = store();
        info.push_str(&mounted(FOREIGN_DEVICE, "/", "/system", "/dev/block/dm-1"));
        info.push_str(&mounted(
            "0:120",
            "/adb/modules/foo/system/bin/tool",
            "/system/bin/tool",
            "KSU",
        ));
        info.push_str(&product("/", "/system/bin/tool"));
        assert_eq!(anchored_exports(&info), names(&["/system/bin/tool"]));
        assert_eq!(unanchored(&info), names(&["/system/bin/tool"]));
    }

    #[test]
    fn name_prefix_is_not_a_parent() {
        let mut info = store();
        info.push_str(&product("/", "/system_ext/lib/tool"));
        info.push_str(&product("/", "/system_extra/bin/tool"));
        assert_eq!(anchored_exports(&info), names(&["/system_ext/lib/tool"]));
        assert_eq!(unanchored(&info), names(&["/system_ext/lib/tool"]));
    }

    #[test]
    fn nested_targets_are_innermost_first() {
        let mut info = store();
        info.push_str(&product("/", "/system/app/Foo"));
        info.push_str(&product("/", "/system/app/Foo/Foo.apk"));
        info.push_str(&product("/", "/system/app/Foo/lib/arm64/libfoo.so"));
        let expected = names(&[
            "/system/app/Foo/lib/arm64/libfoo.so",
            "/system/app/Foo/Foo.apk",
            "/system/app/Foo",
        ]);
        assert_eq!(anchored_exports(&info), expected);
        assert_eq!(unanchored(&info), expected);
    }

    #[test]
    fn stacked_owned_layers_are_each_detached() {
        let mut info = store();
        info.push_str(&product("/", "/system/bin/tool"));
        info.push_str(&product("/", "/system/bin/tool"));
        let expected = names(&["/system/bin/tool", "/system/bin/tool"]);
        assert_eq!(anchored_exports(&info), expected);
        assert_eq!(unanchored(&info), expected);
    }

    #[test]
    fn unowned_top_layer_stops_the_stack() {
        let mut info = store();
        info.push_str(&product("/", "/system/bin/tool"));
        info.push_str(&mounted(FOREIGN_DEVICE, "/", "/system/bin/tool", "other"));
        assert!(anchored_exports(&info).is_empty());
        assert!(unanchored(&info).is_empty());
        let mut stacked = store();
        stacked.push_str(&mounted(FOREIGN_DEVICE, "/", "/system/bin/tool", "other"));
        stacked.push_str(&product("/", "/system/bin/tool"));
        assert_eq!(anchored_exports(&stacked), names(&["/system/bin/tool"]));
        assert_eq!(unanchored(&stacked), names(&["/system/bin/tool"]));
    }

    #[test]
    fn absent_store_keeps_product_source_ownership() {
        let info = product("/system/bin/tool", "/system/bin/tool");
        let expected = names(&["/system/bin/tool"]);
        assert_eq!(anchored_exports(&info), expected);
        assert_eq!(unanchored(&info), expected);
    }

    #[test]
    fn partition_root_layers_are_owned() {
        let mut info = store();
        info.push_str(&product("/", "/system"));
        info.push_str(&product("/", "/vendor"));
        let expected = names(&["/vendor", "/system"]);
        assert_eq!(anchored_exports(&info), expected);
        assert_eq!(unanchored(&info), expected);
    }

    #[test]
    fn partition_layer_leaves_after_the_binds_it_holds() {
        let mut info = store();
        info.push_str(&product("/", "/system"));
        info.push_str(&product("/", "/system/bin/tool"));
        let expected = names(&["/system/bin/tool", "/system"]);
        assert_eq!(anchored_exports(&info), expected);
        assert_eq!(unanchored(&info), expected);
    }

    fn record(target: &str, options: &str) -> String {
        format!("40 1 0:40 / {target} {options} - ext4 /dev/block/dm-1 {options}\n")
    }

    #[test]
    fn covering_mount_is_the_deepest_component_match() {
        let mut info = record("/", "rw");
        info.push_str(&record("/system", "ro,nosuid,nodev"));
        info.push_str(&record("/system/bin", "ro"));
        assert_eq!(
            covering_mount(&info, "/system/bin/tool").unwrap().target,
            "/system/bin"
        );
        assert_eq!(
            covering_mount(&info, "/system/etc/hosts").unwrap().target,
            "/system"
        );
        assert_eq!(covering_mount(&info, "/vendor").unwrap().target, "/");
    }

    #[test]
    fn covering_mount_does_not_match_a_name_prefix() {
        let info = record("/system_ext", "ro");
        assert!(covering_mount(&info, "/system").is_none());
        assert!(covering_mount(&info, "/system_ext/bin").is_some());
    }

    #[test]
    fn covering_mount_prefers_the_topmost_stacked_layer() {
        let mut info = record("/system", "ro,nosuid");
        info.push_str(&record("/system", "ro,nodev"));
        let mount = covering_mount(&info, "/system").unwrap();
        assert_eq!(mount.vfs_option, "ro,nodev");
    }

    #[test]
    fn only_product_source_partition_layers_are_owned() {
        let mut info = store();
        info.push_str(&mounted(FOREIGN_DEVICE, "/", "/system", "KSU"));
        info.push_str(&product("/", "/data"));
        assert!(anchored_exports(&info).is_empty());
        assert!(unanchored(&info).is_empty());
    }

    #[test]
    fn product_runtime_mounts_are_selected() {
        let mut info = store();
        info.push_str(&product("/", &format!("{ROOT}/.egysk/worker")));
        info.push_str(&stored("/egysk/modules", &format!("{ROOT}/.egysk/modules")));
        assert!(anchored_exports(&info).is_empty());
        let expected = names(&[
            &format!("{ROOT}/.egysk/worker"),
            &format!("{ROOT}/.egysk/modules"),
            STORE,
        ]);
        assert_eq!(anchored(&info), expected);
    }

    #[test]
    fn fallback_runtime_outside_product_root_is_not_selected() {
        let mut info = store();
        info.push_str(&product("/", "/debug_ramdisk/.egysk/worker"));
        assert!(anchored_exports(&info).is_empty());
        assert_eq!(anchored(&info), names(&[STORE]));
    }

    #[test]
    fn duplicate_and_malformed_lines_are_handled() {
        let mut info = store();
        info.push_str(&product("/", "/system/bin/other"));
        info.push_str(&product("/", "/system/bin/tool"));
        info.push_str("malformed line\n");
        assert_eq!(
            anchored_exports(&info),
            names(&["/system/bin/tool", "/system/bin/other"])
        );
        assert_eq!(
            unanchored(&info),
            names(&["/system/bin/tool", "/system/bin/other"])
        );
    }

    #[test]
    fn escaped_paths_compare_after_decoding() {
        let mut info = store();
        info.push_str(&product(
            "/system/media/My\\040App/a.png",
            "/system/media/My\\040App/a.png",
        ));
        assert_eq!(
            anchored_exports(&info),
            names(&["/system/media/My App/a.png"])
        );
        assert_eq!(unanchored(&info), names(&["/system/media/My App/a.png"]));
    }

    #[test]
    fn package_backing_is_selected() {
        let mut info = store();
        info.push_str(&package(SELECTED_ESP));
        assert!(anchored_exports(&info).is_empty());
        assert_eq!(unanchored(&info), names(&[ESP]));
    }

    #[test]
    fn foreign_package_and_foreign_root_mounts_are_not_selected() {
        let mut info = store();
        info.push_str(&package("/dev/block/sda1"));
        info.push_str(&package("/dev/egysk/devices/foreign"));
        info.push_str(&mounted(
            FOREIGN_DEVICE,
            "/",
            &format!("{ROOT}/extra"),
            "/dev/block/dm-1",
        ));
        assert!(anchored_exports(&info).is_empty());
        assert!(unanchored(&info).is_empty());
    }

    #[test]
    fn foreign_store_bind_is_never_detached() {
        let info = mounted(FOREIGN_DEVICE, "/foreign", STORE, "/dev/block/foreign");
        assert!(anchored(&info).is_empty());
        assert!(anchored_exports(&info).is_empty());
    }

    #[test]
    fn foreign_store_cannot_authorize_its_own_module_exports() {
        let mut info = mounted(FOREIGN_DEVICE, "/foreign", STORE, "/dev/block/foreign");
        info.push_str(&mounted(
            FOREIGN_DEVICE,
            "/foreign/modules/foo/system/bin/local",
            "/system/bin/local",
            "/dev/block/foreign",
        ));
        info.push_str(&product("/", "/system/bin/proven"));
        assert_eq!(anchored(&info), names(&["/system/bin/proven"]));
        assert_eq!(anchored_exports(&info), names(&["/system/bin/proven"]));
    }

    #[test]
    fn target_store_must_match_the_parent_record() {
        let mut info = mounted("0:177", "/other", STORE, "/dev/block/other");
        info.push_str(&mounted(
            "0:177",
            "/other/modules/foo/system/bin/local",
            "/system/bin/local",
            "/dev/block/other",
        ));
        info.push_str(&product("/", "/system/bin/proven"));
        assert_eq!(anchored(&info), names(&["/system/bin/proven"]));
        assert_eq!(anchored_exports(&info), names(&["/system/bin/proven"]));
    }

    #[test]
    fn ambiguous_or_rootless_parent_store_falls_back() {
        let mut parent = store();
        parent.push_str(&mounted(FOREIGN_DEVICE, "/foreign", STORE, "/dev/block/f"));
        let mut info = store();
        info.push_str(&stored(
            "/egysk/modules/foo/system/bin/local",
            "/system/bin/local",
        ));
        info.push_str(&product("/", "/system/bin/proven"));
        assert_eq!(
            namespace_mounts(&info, &parent),
            names(&["/system/bin/proven"])
        );
        let rootless = format!("10 1 {STORE_DEVICE} / {STORE} rw - ext4 {STORE_SOURCE} rw\n");
        assert_eq!(
            namespace_mounts(&info, &rootless),
            names(&["/system/bin/proven"])
        );
    }

    #[test]
    fn stacked_store_layer_blocks_the_store_detach() {
        let foreign = mounted(FOREIGN_DEVICE, "/foreign", STORE, "/dev/block/foreign");
        let mut info = store();
        info.push_str(&foreign);
        info.push_str(&stored(
            "/egysk/modules/foo/system/bin/local",
            "/system/bin/local",
        ));
        assert_eq!(anchored(&info), names(&["/system/bin/local"]));
    }
}
