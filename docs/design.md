# Design rationale and evidence

This document records why the implementation has its current shape, and which sources support
each statement. It describes the current state, not a change history.

## Pinned inputs

|Input|Pin|Provenance|
|---|---|---|
|Magisk source|`e8915d9db15f5aae93973ffe65068e34df375a6a` (`v31.0-81-ge8915d9db`)|`PIN` in `magisk/scripts/magisk.py`. The submodule carries the same commit.|
|Magisk patch series|`magisk/patches/Magisk/series`|The materializer applies it with exact context.|
|ONDK|`r30.1`|`VERSION` in `tools/bootstrap_ondk.py`. SHA-256 per host: `a05e40a2…0289` (linux), `a2aff59a…a13e` (darwin), `5743de90…276a` (windows).|
|BusyBox prebuilt|`busybox-1.36.1.1.zip` from `topjohnwu/magisk-files`|SHA-256 `b4d0551feabaf314e53c79316c980e8f66432e9fb91a69dbbf10a93564b40951` in `tools/bootstrap_busybox.py`. The module comment records that the pin and ABI member names match Magisk `e8915d9`.|
|Android NDK for static Bionic|r29, `Pkg.Revision = 29.0.14206865`|`magisk/scripts/magisk.py` checks `Pkg.Revision = 29.` and requires the API-35 sysroot `libc.a`. CI verifies the r29 archive with SHA-256 `4abbbcdc842f3d4879206e9695d52709603e52dd68d3c1fff04b3b5e7a308ecf` (`.github/workflows/daemon.yml`). That workflow comment also records Google's listed size of 783549481 bytes and SHA-1 `87e2bb7e9be5d6a1c6cdf5ec40dd4e0c6d07c30b` for the same archive.|
|`libc` crate|Git revision `1a661633f11a261ff9c00b40875ef72f1cbec818`|`[patch.crates-io]` in `Cargo.toml`.|

## Architecture decisions

### Magisk keeps lifecycle ownership

One stage runner, one inventory, one magic-mount implementation, and one SELinux path exist.
Egysk adds storage adaptation and early hooks. A second lifecycle manager would duplicate
serialization, stage ordering, and policy loading, and the results would diverge.

Evidence: `magisk/runtime/src/lib.rs` states that lifecycle, inventory, init RC, policy, and
projection belong to Magisk. `magisk/runtime/src/storage.rs` states that the adapter is not a
lifecycle dispatcher. `egysk-runtime` is a library, and the product has no helper ioctl ABI.

### The archive carries no `/init`

Magiskinit backs up and restores the stock initializer of the current boot. Bundling another
ROM's initializer would bind the product to a foreign first-stage implementation.

Evidence: `magisk/product/init.rs` writes `/.egysk-backup/.magisk` and resolves a relative `/init`
symlink before the backup, so moving the link into `.egysk-backup` cannot change its target.

### `RECOVERYMODE` stays `false`

`RECOVERYMODE` is upstream's recovery-as-boot installation flag. The bootloader already selected
normal or recovery ramdisk. Deriving the flag from the current mode would select the legacy
system-as-root path and can wait forever for a system partition under `rootwait`.

Evidence: the comment in `magisk/product/init.rs` and the `skip_initramfs` branch in
`magisk/patches/Magisk/0004-native-init-and-early-hooks.patch`.

### Recovery intercepts the direct second-stage exec

Recovery does not switch root. The patch intercepts the initializer's direct second-stage exec and
selects `/init.real` when present, which mirrors upstream two-stage detection. Otherwise the
target is `/init`.

Evidence: `0004-native-init-and-early-hooks.patch` adds `hexpatch_init_for_second_stage(true)` and
the `/init.real` selection in `native/src/init/twostage.rs`.

### Two backing kinds, no ephemeral fallback

Block backing calls a helper that owns a volume. Filesystem backing adopts a mount that stock init
already created. Both paths fail on a mismatch instead of falling back to a directory or a RAM
store. A fallback would hide a broken platform contract and produce a boot that appears healthy
without persistent module state.

Evidence: `magisk/runtime/src/bootstrap.rs` (helper protocol, `adopt_filesystem`, store-bind
ownership checks) and `magisk/runtime/src/storage.rs` (filesystem early path returns no modules).

### The ESP is not an overlay lower

The ESP is VFAT. OverlayFS requires a lower with the metadata semantics that module content
expects. The design copies ESP content to verified read-only tmpfs lowers, then mounts a
per-generation upper/work pair on the shared store.

`metacopy=on` keeps `chmod`, `chown`, and label changes from copying unchanged data. The mount
sequence checks the effective metacopy mode and fails on a silent downgrade, because a downgrade
would copy whole files into the store.

Evidence: `magisk/runtime/src/store.rs` (`require_metacopy`, the overlay option string).

### Generation identity follows package bytes

Generation identity binds deltas to package content, not to recreated tmpfs inode numbers.
Unchanged generations keep their edits, and a package change gets a distinct upper/work pair.
This preserves user edits across boots without reusing an active lower.

Evidence: `magisk/runtime/src/store.rs` module comment on recoverable activation and content
identity, plus the generation marker handling in `current_generation`.

### Lower-cache reclamation is PID1-only

Effective lowers are boot-local tmpfs copies under `/dev/egysk/lowers`. Earlier on-disk lower
copies can remain on the store under `overlays/<id>/<generation>/lower` and `.lower-incomplete`.
PID 1 removes those copies for block backing before any view exists. It never removes upper, work,
local packages, preferences, or durable state. Filesystem backing becomes available only after
stock init starts, so the daemon does not reclaim those copies. The daemon still refuses activation
while effective views are mounted.

Evidence: `magisk/runtime/src/storage.rs` comments at the reclamation call, and
`magisk/runtime/src/store.rs` activation checks. `reclaim_lower_caches` requires PID 1 (`magisk/runtime/src/store.rs`).

### Target-namespace cleanup follows the module store

A DenyList target process must not keep the module projection or the product runtime. The
selector matches the module store, not a path prefix. A mount holds module content when its
source is `egysk` or when its device matches the store filesystem and its root is below the
store's module directory. A mount belongs to the product runtime when its source is `egysk` or
when it is a module or state mount of the store filesystem.

A fixed prefix cannot serve. The kernel records the source filesystem and the subtree path in
`mountinfo`. A bind mount of a module file on the store therefore reports the store device and
the in-filesystem module path, not the product name. The source `egysk` covers only the overlay
and tmpfs mounts that this product creates directly.

Evidence: user-namespace probes on Linux 7.3.0-5-cachyos recorded both cases. A tmpfs mounted
with source `/dev/block/dm-9` reported that source and the root `/egysk/modules/foo` for a bind
of a subdirectory. An overlay mounted with source `egysk` reported source `egysk` and the root
`/system/bin/tool` for a bind of a file inside it. `mounts::namespace_mounts`,
`mounts::projection_exports`, and their unit tests record the selection rules
(`magisk/runtime/src/mounts.rs`).

The target namespace cannot prove its own store. mountinfo names a store mount by its target,
its device, and its root, and a foreign mount at `/dev/egysk/store` can carry any of those
fields. `revert_unmount` therefore anchors identity to the parent namespace. It reads
`/proc/1/mountinfo` and accepts the store only from a single record at the store target. A target
store bind, module view, or state bind counts as owned only when its device and root agree with
that record. Two parent records, a missing parent record, a parent store whose root is `/`, or a
target store that disagrees all fall back to product-source mounts and the package mount. An
unowned layer on a target stops the walk, so a foreign store stacked over the real one keeps the
store.

Both selectors anchor to the parent record, so a foreign store cannot authorize its own exports.
The early projection runs in the PID 1 namespace, so its anchor equals its own view. It detaches
export mounts only, because the module views are its source, and it never selects the store
target.

The target-namespace cleanup detaches exports and the product runtime mounts at or below
`/dev/egysk`, which covers the store bind, the views, the lowers, the worker, the module mirror,
and the state binds when the parent record confirms them. It also detaches the package mount at
`/dev/esp` when its source is exactly the selected ESP device node. Platform mounts and a foreign
installation stay. The deepest target leaves first, so a directory mount does not detach an
exported file mount in the same pass. `magisk --denylist exec` runs the same rules in its private
namespace, so a command there loses a matching store copy as well.

A target holds one mount per layer, and an older layer can stay under a newer one. The selector
returns one entry for each owned layer. It stops at an unowned layer on the same target, because
a detach removes the topmost mount of a target and cannot skip that layer. This keeps an
unrelated mount and the layers below it.

An unreadable mount list fails the early projection. The namespace cleanup logs the failure and
detaches nothing, because it cannot prove ownership of any target.

A switched namespace is not an isolated namespace. A mount namespace that copies a shared mount
joins that peer group, and the kernel then forwards `mount` and `umount` events between the
peers. A detach in the copy can therefore remove the mount from the parent namespace.

Evidence: `mount_namespaces(7)` states that a shared mount propagates `mount(2)` and `umount(2)`
events to the other members of its peer group, and that an event under a slave mount does not
propagate to a peer. A disposable user-namespace probe recorded the effect on Linux
7.3.0-5-cachyos. A peer namespace that detached a mount it created removed the parent's copy. A
peer namespace that ran `mount --make-rslave /` before the detach kept the parent's copy, and an
inbound mount still arrived in the slave.

`revert_unmount` therefore refuses a mount namespace that matches PID 1's namespace, converts the
namespace tree to recursive slave, and returns on either failure
(`magisk/patches/Magisk/0002-lifecycle-and-projection-adapters.patch`). The daemon namespace and
its shared peers stay unchanged, and Android keeps its inbound propagation.

### Visibility boundaries

The cleanup removes mounts. It does not remove the evidence that other interfaces expose, and
it does not falsify it:

- The plain directories of `/dev/egysk` (`bin`, `rd`) are entries in the `/dev` tmpfs, not
  mounts. Their labels decide whether another domain can read them.
- The store bind leaves the namespace only when the parent record confirms its device and root.
  A mismatched or stacked store target keeps it, and the store directory mode and its label
  remain the boundary.
- `/proc/cmdline` keeps `rdinit=/egyskinit` and any debug key that the bootloader supplied.
- `init.svc.*` properties name the running service to any property reader.
- A process outside a DenyList target keeps every mount. Per-application hiding needs a target
  entry, because the product ships no manager application and injects no Zygisk code.
- Attestation inputs stay untouched. The product does not rewrite verified-boot state and does
  not report a boot state that the platform did not produce.

Changes to those observations need a kernel component, an injection path, or a trust-anchor
change. A KernelSU metamodule, SUSFS, and a Zygisk module each add one of those. This repository
holds no pinned source for them, and a kernel module needs KMI admission in `maos-lkms`.

Two projector variants share the same limit:

- `backslashxx/mountify` builds one shared tmpfs OverlayFS tree instead of per-file bind mounts.
  It needs `CONFIG_OVERLAY_FS` and recommends `CONFIG_TMPFS_XATTR`, assumes `/data/adb/modules`,
  copies files and their SELinux labels, and expects the Magisk `skip_mountify` marker. It
  replaces projection, so it would need the store/ESP inventory, `modules_order`, valued flags,
  and the two projection passes of this product.
- [`Hybrid-Mount/meta-hybrid_mount`](https://github.com/Hybrid-Mount/meta-hybrid_mount/tree/89f328fd945c72794a6505d779f6d85834d131a3)
  selects overlay, magic, or VFS per path. Its VFS provider marks synthetic inodes `S_PRIVATE`.
  In the 6.12 DDK kernel, `security_inode_permission` and SELinux `inode_has_perm` skip checks
  for those inodes. Clearing the flag is not enough: forwarding the source label does not
  preserve the replaced target's label. The keyring also accepts raw `CAP_SYS_ADMIN` callers
  that can bypass a userspace policy. The provider is not included; a safe port needs
  kernel-enforced target and source authorization for every rule.

An opt-in read-only variant of the shared-staging model is integrated as `overlay_readonly`
(`magisk/runtime/src/projection.rs`, patch `0005`). The module sets the marker, the product
stages immutable copies under its own tmpfs, and one read-only layer per partition is mounted
above the real partition. Writes never reach a module view, and a rejected plan stops the boot
before any mount. The layer covers a whole partition, so admission requires the covering mount
of that partition to be read-only already; a writable covering mount, including recovery's
ramdisk `/system`, is rejected instead of becoming silently read-only. Magic Mount stays the
default, so the rest of Mountify's model is still not integrated: its own inventory, its own
ordering, and read-only projection without a per-module opt-in.

### The KernelSU Magic Mount metamodule is not a drop-in projector

The standalone metamodule (`Tools-cx-app/meta-magic_mount-rs`, revision `fe7a1e7`) replaces a
KernelSU userspace mount service. It installs only when `KSU` or `APATCH` is present.
`metamount.sh` runs `meta-mm` and then calls `/data/adb/ksud kernel notify-module-mounted`. It
reads `/data/adb/magic_mount/config.toml` with `mountsource = "KSU"` and scans
`/data/adb/modules` through the KernelSU supercall interface (`module/customize.sh`,
`module/metamount.sh`, `module/config.toml`, `src/ksucalls.rs`, `src/scanner.rs`).

A replacement of this product's projector would need all of these contracts:

- Module inventory from the store and ESP views, with `modules_order` and the valued marker
  files.
- The early `rdinit` projection and the final Android projection, with removal of the early
  exports.
- The product mount source and the target-namespace cleanup that this document describes.
- Fatal handling for a failed mount or stage.
- A KernelSU or APatch kernel interface, or a replacement for its supercall and notification
  path.

The upstream code implements a different inventory, configuration path, mount source, and
failure policy. A replacement is therefore a port with device qualification, not a pin change or
a drop-in. The license is GPL-3.0, which matches this repository. This repository keeps Magisk's
projector, so a KernelSU metamodule would add a second one.

The same limits apply to a Zygisk module as the visibility mechanism. Zygisk Assistant needs
Zygisk turned on and `Enforce DenyList` turned off, and then uses the deny list as its target list
(`snake-4/Zygisk-Assistant`, revision `4c1842f`, `README.md`). It hides from a Zygisk hook.
Egysk injects no Zygisk code, so the module has no hook to attach
(`magisk/patches/Magisk/0002-lifecycle-and-projection-adapters.patch`).

### Fatal failures stay fatal

A fatal early failure writes to kmsg, the boot log, and the store, then panics or exits. A quiet
reboot would discard the cause and could look like an ordinary user reboot. Diagnostic boots also
set `androidboot.init_fatal_panic=true` for init's own failures.

Evidence: `fatal_boot` in `magisk/runtime/src/lib.rs`. The `sysrq-trigger` path is used only by
non-PID1 workers.

### Debug shell SU is a boot input

`androidboot.egysk.debug=1` grants UID 2000 the upstream `Allow` row. The daemon reconciles the row
on every start, so a previous debug boot cannot leave access behind. The value is not a persisted
product setting, and adb behavior stays stock.

Evidence: `debug_boot`, `shell_policy_sql`, and `configure_shell_policy` in
`magisk/runtime/src/lib.rs` and `magisk/product/dispatch.rs`.

### Virtual-only modules need a named owner

A module that replaces the boot-control service must not run beside the device's native boot HAL.
Module admission happens at PID 1, before Android's properties and before any later identity phase,
so a property read cannot decide it. The package therefore names one early identity provider, and
Egysk runs it after tool staging and before backing storage and discovery. The provider answers the
`early-identity` command with one owner: `virtual`, `physical`, `unmanaged` for a recorded direct
boot, or `missing` for no dispatch record. Egysk admits a module that contains a `virtual_only`
file only for the `virtual` owner. Every other owner, and an absent provider field, admits nothing,
so a missing identity cannot install a replacement HAL, its policy, or its RC. Egysk records the
answer in the boot snapshot and reuses it for the Android stage; it does not count or bless boot
entries, and it does not derive a second owner from the kernel command line or bootconfig.

A configured provider that fails, or a record with an unknown owner or unknown field, stops the
boot. Safe mode and charger boot discover no module, so they make no admission decision and do not
run the provider.

Evidence: `identity::read` (`magisk/runtime/src/identity.rs`), `virtual_only_skipped_by` and
`virtual_only_skipped_for` in `magisk/runtime/src/store.rs`, the provider call and admission check
in `magisk/runtime/src/storage.rs`, and the test
`virtual_only_modules_are_admitted_only_for_the_virtual_owner` in
`magisk/runtime/tests/boot_contract.rs`.

### The boot-payload module runs before first-stage mounts

Bootstrap kernel modules must exist before first-stage consumers run. `egyskinit` therefore runs
the `lkm-loader` `pid1.sh` before it prepares storage, and the loader stops at the first failing
module. This step runs before boot-mode classification, so `recovery_ok`, `disable`, `remove`, and safe mode do not apply to it. Those flags govern only discovered ESP and local modules, which run later through `rdinit` and the Magisk module stages.

Evidence: `magisk/product/init.rs` (execution before `storage::open_early`) and
`modules/lkm-loader/src/main.rs` (list parsing before loading, first failure stops the list).

### BusyBox comes from Magisk's pin

The direct packager obtains BusyBox from Magisk's own prebuilt archive and verifies the archive
SHA-256 on every use. An operator-supplied binary would add an unverified input to the early boot
path.

Evidence: `tools/bootstrap_busybox.py` and the call in `magisk/scripts/package_direct.py`.

### ONDK builds, NDK r29 links statically

ONDK r30.1 supplies the compiler and Rust toolchain. Magiskinit still needs the full static Bionic
from NDK r29, because early filesystem and process hooks exceed the small libc subset in crt0.
ONDK r30.1 cannot serve that input, because its `libc.a` embeds a Rust standard library and the
link reports a duplicate `rust_eh_personality` symbol.

Evidence: `magisk/scripts/magisk.py` checks, the comment in `.github/workflows/daemon.yml`, and the
`build_native` recipe in `justfile`.

### Archive framing

The archive member uses the LZ4 legacy format with magic `0x184c2102`. Consumers of the produced
package require that framing, and `egysk-build-cpio --legacy-lz4` selects it. A host test
round-trips a full block and a partial final block (`tools/egysk-tools/src/tests.rs`).

## Evidence boundaries

Observations in this repository support source and host-test claims. They do not support
statements about a booted device.

- Host tests cover archive framing, KMI selection, module admission, boot configuration parsing,
  boot-mode classification, virtual-only admission, storage contracts, mount namespace
  selection, and log rotation. See `tools/egysk-tools/src/tests.rs`,
  `magisk/runtime/tests/boot_contract.rs`, and the test modules in `magisk/runtime/src`.
- A successful build, a passing host test, or a successful archive admission shows that the
  exercised inputs satisfy the checked conditions. It does not show that Android boots.
- Enforcing operation, credential enrollment and unlock, sealed projection, fastbootd, recovery boot, and OTA behavior need consumer evidence.
  Each also needs evidence from the platform owner.
- This repository holds no device qualification records. Report device status only from the
  platform owner's records, and report emulator exercise separately from physical-device
  qualification.
- Exact-KMI admission happens at install or OTA time in `maos-lkms`. Egysk does not recheck a
  module against the shared package's initial KMI on every boot.
- A passing selector test shows which mount targets a cleanup names. It does not show that a
  device process, a scanner, or an attestation path observes no module content. This repository
  makes no detection-free claim, and it adds no attestation override.
