//! Read-only OverlayFS projection for modules that opt in.
//!
//! Magic Mount binds every module file and keeps writes inside the module's
//! effective view. A module can instead set `overlay_readonly` to `1` in its own
//! directory or in `module.prop`. Such a module is projected through one
//! OverlayFS mount per partition, with an immutable staged layer above the real
//! partition. The projected content is read-only: a write through it fails with
//! `EROFS`, and nothing is copied back to the module. There is no fallback: a
//! module without the marker keeps Magic Mount, and a rejected plan stops the
//! boot before any projection mount exists.
//!
//! This module is the planner only. It reads the module views and the real
//! partitions, then returns the staged tree operations and the overlay mounts.
//! The native projector executes the returned operations.
//!
//! Parity with the native Magic Mount projection, which this planner must not
//! silently change for the same input:
//!
//! - a module directory merges into an existing real directory;
//! - a non-directory `.replace` entry replaces the whole parent directory, so
//!   real children the module does not provide are hidden;
//! - a character device with device number 0/0 is a whiteout and hides the real
//!   name;
//! - symlinks are recreated, and a file or symlink hides a real name of any
//!   type;
//! - a node whose real target does not exist directly below a partition root is
//!   rejected, as the native projector rejects it;
//! - owner, mode, and SELinux label come from the real target when it exists.
//!
//! Rejected instead of projected, because the native projector cannot express
//! them and an overlay would change the result: a module directory over a real
//! non-directory, a module file over a real directory, and any node type other
//! than directory, regular file, symlink, and whiteout.

use anyhow::{Context, Result, bail, ensure};
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

/// Marker file in a module directory. The value must be `1` or `true`.
pub const OVERLAY_MARKER: &str = "overlay_readonly";

/// Partitions extracted out of a module's `system` tree when the real partition
/// exists, matching the native projection.
pub const SECONDARY_PARTITIONS: [&str; 3] = ["vendor", "product", "system_ext"];

/// One staged tree operation. Every `to` path is inside the staged layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Create a directory and copy owner, mode, and label from `attr`.
    Dir { to: PathBuf, attr: PathBuf },
    /// Copy file data, then copy owner, mode, and label from `attr`.
    Copy {
        from: PathBuf,
        to: PathBuf,
        attr: PathBuf,
    },
    /// Create a symlink and, when `attr` is set, copy owner and label from it.
    Link {
        to: PathBuf,
        target: String,
        attr: Option<PathBuf>,
    },
    /// Create a character device 0/0, which OverlayFS reads as a whiteout.
    Whiteout { to: PathBuf },
}

impl Op {
    /// The staged path this operation creates.
    pub fn to(&self) -> &Path {
        match self {
            Op::Dir { to, .. }
            | Op::Copy { to, .. }
            | Op::Link { to, .. }
            | Op::Whiteout { to } => to,
        }
    }
}

/// One read-only OverlayFS mount over a partition root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// Android path the overlay is mounted on, for example `/system`.
    pub target: String,
    /// Staged layer root, the highest-priority lower directory.
    pub staged: PathBuf,
    /// Real partition root, the lowest-priority lower directory.
    pub real: PathBuf,
}

impl Mount {
    /// OverlayFS mount data. The staged layer is searched before the real root.
    ///
    /// A path that OverlayFS would split as another option value is rejected,
    /// because the mount would then read a different layer than this plan.
    pub fn data(&self) -> Result<String> {
        for path in [&self.staged, &self.real] {
            let text = path
                .to_str()
                .with_context(|| format!("overlay path is not UTF-8: {}", path.display()))?;
            ensure!(
                text.starts_with('/'),
                "overlay path is not absolute: {text}"
            );
            ensure!(
                !text.contains([':', ',', '\0']),
                "overlay path cannot be an overlay option value: {text}"
            );
        }
        Ok(format!(
            "lowerdir={}:{}",
            self.staged.display(),
            self.real.display()
        ))
    }
}

/// Mount flags a projection layer inherits from the partition it covers.
///
/// A projection never adds a restriction that the real partition does not have,
/// and never drops one it has. `ReadOnly` is always present: the projected
/// content is read-only by contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flag {
    ReadOnly,
    NoSuid,
    NoDev,
    NoExec,
    NoAtime,
    NoDirAtime,
    NoSymFollow,
}

/// Restriction flags inherited from a covering mount's options.
///
/// [`admission`] decides whether a layer may exist at all; this mapping only
/// mirrors the restrictions, so a projection never adds one the mount does not
/// have and never drops one it has.
pub fn flags_for(vfs_option: &str) -> Vec<Flag> {
    let mut flags = vec![Flag::ReadOnly];
    let options: BTreeSet<&str> = vfs_option.split(',').map(str::trim).collect();
    if options.contains("nosuid") {
        flags.push(Flag::NoSuid);
    }
    if options.contains("nodev") {
        flags.push(Flag::NoDev);
    }
    if options.contains("noexec") {
        flags.push(Flag::NoExec);
    }
    if options.contains("noatime") {
        flags.push(Flag::NoAtime);
    }
    if options.contains("nodiratime") {
        flags.push(Flag::NoDirAtime);
    }
    if options.contains("nosymfollow") {
        flags.push(Flag::NoSymFollow);
    }
    flags
}

/// First path where the two projection methods overlap.
///
/// A Magic Mount bind sits above every overlay layer, so a shared path would
/// show the bind whatever the module order says. A bind on a directory, which
/// Magic Mount creates for a replaced directory, also covers everything below
/// it, and an overlay whiteout hides everything below its name. Both
/// directions are therefore a conflict. `overlay` must be sorted, as
/// `Plan::targets` returns it.
pub fn first_conflict(magic: &[String], overlay: &[String]) -> Option<String> {
    for path in overlay {
        if magic
            .iter()
            .any(|claim| covers(claim, path) || covers(path, claim))
        {
            return Some(path.clone());
        }
    }
    None
}

/// Whether `ancestor` is `path` or one of its parent directories.
fn covers(ancestor: &str, path: &str) -> bool {
    path == ancestor
        || (path.len() > ancestor.len()
            && path.starts_with(ancestor)
            && path.as_bytes()[ancestor.len()] == b'/')
}

/// Flags for a projection layer over `target`, or a rejection.
///
/// The layer covers a whole partition, so the covering mount must already be
/// read-only: a read-only layer over a writable mount would change unrelated
/// paths that the mount still serves writable. The covering mount is the deepest
/// mount record over the canonical target, matched by path components, so a
/// `/system` directory on a writable root does not inherit root's options and a
/// `/system_ext` record does not cover `/system`. A missing record, an
/// unreadable target, or a writable record is rejected before the caller stages
/// anything. Restrictions of the covering mount are inherited, never added or
/// dropped.
///
/// The canonical target must equal the declared partition root. An alias would
/// be mounted at one path while the mount record and both product mount
/// selectors name the other, so the early layer could survive cleanup. An alias
/// is rejected before staging, as a missing or writable covering mount is.
pub fn admission(mountinfo: &str, target: &Path) -> Result<Vec<Flag>> {
    let canonical = fs::canonicalize(target)
        .with_context(|| format!("resolve overlay target {}", target.display()))?;
    ensure!(
        canonical.as_path() == target,
        "overlay target {} resolves to {}; partition-root aliases are unsupported",
        target.display(),
        canonical.display()
    );
    let canonical = canonical
        .to_str()
        .with_context(|| format!("overlay target is not UTF-8: {}", target.display()))?;
    let covering = crate::mounts::covering_mount(mountinfo, canonical)
        .with_context(|| format!("overlay target {canonical} has no covering mount"))?;
    let options: BTreeSet<&str> = covering.vfs_option.split(',').map(str::trim).collect();
    ensure!(
        options.contains("ro") && !options.contains("rw"),
        "overlay target {canonical} is covered by writable mount {} ({})",
        covering.target,
        covering.vfs_option
    );
    Ok(flags_for(&covering.vfs_option))
}

/// One module that opted into the read-only overlay projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    /// Module identifier, used in messages only.
    pub id: String,
    /// Effective `system` tree of the module view.
    pub content: PathBuf,
}

impl Module {
    pub fn new(id: impl Into<String>, content: impl Into<PathBuf>) -> Self {
        Module {
            id: id.into(),
            content: content.into(),
        }
    }
}

/// Staged operations and overlay mounts for one projection stage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub ops: Vec<Op>,
    pub mounts: Vec<Mount>,
}

impl Plan {
    /// Absolute Android paths this projection provides or hides.
    ///
    /// Directory creation is not a claim: both projection methods merge real
    /// directories, so only files, symlinks, and whiteouts can collide. A
    /// whiteout is a claim because a bind on the same path would restore a name
    /// this projection removes.
    pub fn targets(&self) -> Vec<String> {
        let mut targets = Vec::new();
        for op in &self.ops {
            if matches!(op, Op::Dir { .. }) {
                continue;
            }
            let path = op.to();
            let Some(mount) = self
                .mounts
                .iter()
                .find(|mount| path.starts_with(&mount.staged) && path != mount.staged)
            else {
                continue;
            };
            let relative = path
                .strip_prefix(&mount.staged)
                .expect("mount root checked above");
            let mut target = mount.target.clone();
            for component in relative.components() {
                if let Component::Normal(name) = component {
                    target.push('/');
                    target.push_str(&name.to_string_lossy());
                }
            }
            targets.push(target);
        }
        targets.sort();
        targets.dedup();
        targets
    }
}

/// One merged module node.
struct Node {
    kind: Kind,
    /// Module view path the content comes from.
    source: PathBuf,
    /// Set when a non-directory `.replace` entry covers this directory.
    replaced: bool,
    children: BTreeMap<String, Node>,
}

enum Kind {
    Dir,
    File,
    Symlink(String),
    Whiteout,
}

impl Node {
    fn dir(source: PathBuf) -> Node {
        Node {
            kind: Kind::Dir,
            source,
            replaced: false,
            children: BTreeMap::new(),
        }
    }

    fn file(source: PathBuf) -> Node {
        Node {
            kind: Kind::File,
            source,
            replaced: false,
            children: BTreeMap::new(),
        }
    }

    fn whiteout(source: PathBuf) -> Node {
        Node {
            kind: Kind::Whiteout,
            source,
            replaced: false,
            children: BTreeMap::new(),
        }
    }
}

/// Plan the read-only overlay projection.
///
/// `real_base` is `/` on a device and a fixture root in tests. `staging` is the
/// product-owned tmpfs directory the staged layers are written to. Modules are
/// merged in the order they are given: the first provider of a name wins, as in
/// the native projection.
pub fn plan(
    real_base: &Path,
    staging: &Path,
    partitions: &[&str],
    modules: &[Module],
) -> Result<Plan> {
    ensure!(!modules.is_empty(), "overlay projection without modules");
    let mut root = Node::dir(PathBuf::new());
    for module in modules {
        if !module.content.is_dir() {
            continue;
        }
        merge(&mut root, &module.content).with_context(|| {
            format!("module {} content: {}", module.id, module.content.display())
        })?;
    }

    let mut trees: BTreeMap<String, Node> = BTreeMap::new();
    let mut system = Node::dir(PathBuf::new());
    // A non-directory `.replace` at the module `system` root replaces the
    // `/system` partition root itself. It belongs to the system tree, which the
    // loop below rejects, not to the content merged under it.
    let replaced_root = root.replaced;
    for (name, node) in root.children {
        let extracted = matches!(&node.kind, Kind::Dir)
            && partitions.contains(&name.as_str())
            && real_base.join(&name).is_dir();
        if extracted {
            trees.insert(name, node);
        } else {
            system.children.insert(name, node);
        }
    }
    system.replaced = replaced_root;
    trees.insert("system".to_string(), system);

    let mut plan = Plan::default();
    for (name, node) in trees {
        // The native projector rejects a root replacement: the partition root
        // is never rebuilt, so a marker there has no meaning it can honor.
        if node.replaced {
            bail!("Unsupported root projection: /{name}");
        }
        if node.children.is_empty() {
            continue;
        }
        let real_root = real_base.join(&name);
        ensure!(
            real_root.is_dir(),
            "overlay projection requires a backed /{name}"
        );
        let staged_root = staging.join(&name);
        plan.ops.push(Op::Dir {
            to: staged_root.clone(),
            attr: real_root.clone(),
        });
        for (child, entry) in &node.children {
            let real = real_root.join(child);
            match fs::symlink_metadata(&real) {
                Ok(meta) => ensure!(
                    !meta.file_type().is_symlink(),
                    "Unsupported root projection: {}",
                    real.display()
                ),
                Err(_) => bail!("Unsupported root projection: {}", real.display()),
            }
            emit(entry, &real, &staged_root.join(child), &mut plan.ops)?;
        }
        plan.mounts.push(Mount {
            target: format!("/{name}"),
            staged: staged_root,
            real: real_root,
        });
    }
    Ok(plan)
}

fn merge(node: &mut Node, dir: &Path) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("open {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let name = utf8(entry.file_name())?;
        ensure!(
            name != "." && name != ".." && !name.is_empty() && !name.contains('\0'),
            "invalid module entry name in {}",
            dir.display()
        );
        let path = entry.path();
        let meta =
            fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        let file_type = meta.file_type();
        // A directory named `.replace` is ordinary content: only a
        // non-directory entry marks the parent as replaced.
        if name == ".replace" && !file_type.is_dir() {
            node.replaced = true;
            continue;
        }
        if file_type.is_dir() {
            match node.children.entry(name) {
                Entry::Occupied(mut existing) => {
                    // A name this plan already merged from a higher-priority
                    // module is decided; the native projection drops this
                    // subtree too when the earlier node is not a directory.
                    if matches!(existing.get().kind, Kind::Dir) {
                        merge(existing.get_mut(), &path)?;
                    }
                }
                Entry::Vacant(vacant) => {
                    let mut child = Node::dir(path.clone());
                    merge(&mut child, &path)?;
                    vacant.insert(child);
                }
            }
            continue;
        }
        if node.children.contains_key(&name) {
            continue;
        }
        if file_type.is_symlink() {
            let target =
                fs::read_link(&path).with_context(|| format!("read link {}", path.display()))?;
            let target = utf8(target.into_os_string())?;
            node.children.insert(
                name,
                Node {
                    kind: Kind::Symlink(target),
                    source: path,
                    replaced: false,
                    children: BTreeMap::new(),
                },
            );
        } else if file_type.is_file() {
            node.children.insert(name, Node::file(path));
        } else if file_type.is_char_device() && meta.rdev() == 0 {
            node.children.insert(name, Node::whiteout(path));
        } else {
            bail!("unsupported module node type: {}", path.display());
        }
    }
    Ok(())
}

fn emit(node: &Node, real: &Path, staged: &Path, ops: &mut Vec<Op>) -> Result<()> {
    let target = fs::symlink_metadata(real).ok();
    match &node.kind {
        Kind::Dir => {
            if let Some(meta) = &target {
                ensure!(
                    meta.is_dir(),
                    "module directory over non-directory: {}",
                    real.display()
                );
            }
            let attr = match &target {
                Some(_) => real.to_path_buf(),
                None => nearest_existing(real)?,
            };
            ops.push(Op::Dir {
                to: staged.to_path_buf(),
                attr,
            });
            for (name, child) in &node.children {
                emit(child, &real.join(name), &staged.join(name), ops)?;
            }
            // Replacing a directory hides every real child the module does not
            // provide, which is what skipping the mirror does natively.
            if node.replaced && target.as_ref().is_some_and(fs::Metadata::is_dir) {
                for entry in
                    fs::read_dir(real).with_context(|| format!("read {}", real.display()))?
                {
                    let entry = entry?;
                    let name = utf8(entry.file_name())?;
                    if node.children.contains_key(&name) {
                        continue;
                    }
                    ops.push(Op::Whiteout {
                        to: staged.join(&name),
                    });
                }
            }
        }
        Kind::File => {
            if let Some(meta) = &target {
                ensure!(
                    !meta.is_dir(),
                    "module file over directory: {}",
                    real.display()
                );
            }
            // The label, owner, and mode of a path that exists come from the
            // path itself, as the native projection copies them onto the module
            // file. A path that does not exist keeps the module's own metadata.
            let attr = if target.is_some() {
                real.to_path_buf()
            } else {
                node.source.clone()
            };
            ops.push(Op::Copy {
                from: node.source.clone(),
                to: staged.to_path_buf(),
                attr,
            });
        }
        Kind::Symlink(link) => ops.push(Op::Link {
            to: staged.to_path_buf(),
            target: link.clone(),
            attr: target.map(|_| real.to_path_buf()),
        }),
        Kind::Whiteout => ops.push(Op::Whiteout {
            to: staged.to_path_buf(),
        }),
    }
    Ok(())
}

fn nearest_existing(path: &Path) -> Result<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|ancestor| fs::symlink_metadata(ancestor).is_ok())
        .map(Path::to_path_buf)
        .with_context(|| format!("no existing ancestor for {}", path.display()))
}

fn utf8(value: std::ffi::OsString) -> Result<String> {
    match value.to_str() {
        Some(text) => Ok(text.to_string()),
        None => bail!("not UTF-8: {}", value.to_string_lossy()),
    }
}
