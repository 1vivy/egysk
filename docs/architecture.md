# Architecture and runtime contract

This document describes the checked-in implementation. Source paths give provenance for each
statement.

## Product shape

Egysk is a set of patches and native adapters over one pinned Magisk revision. Magisk owns
initialization, SELinux integration, module inventory order, stage execution, and magic mount.
Egysk owns the storage adapter, boot-payload module loading, and archive production.

The product installs no manager application and no kernel helper. `APP_PACKAGE_NAME` is
`org.egysk.no_manager` and `EGYSK_PRODUCT` is true in `magisk/product/identity.json`.

## Artifacts

|Artifact|Source|Role|
|---|---|---|
|`egyskinit`|Magisk `magiskinit` plus `magisk/product/init.rs`|PID 1 entry from `rdinit=`. Runs the boot-payload module, stages module scripts, and backs up the stock initializer.|
|`egyskd`|Magisk native daemon plus `magisk/product/dispatch.rs`|Second-stage daemon. Reconciles shell policy, runs module hooks, and blesses a counted boot entry.|
|`magiskpolicy`|Upstream Magisk|Compiles policy rules.|
|`init-ld`|Upstream Magisk `libinit-ld.so`|Preload library for init interception.|
|`egysk-runtime`|`magisk/runtime`|Linked storage, inventory, and script library. It is not a process.|
|`egysk-build-cpio`|`tools/egysk-tools`|Host archive builder with ELF and module admission checks.|
|`lkm-loader`|`modules/lkm-loader`|Static kernel-module loader used at PID 1.|

`egysk-runtime` exports the store operations that the daemon calls for install, update, removal,
rollback, and enable state (`magisk/runtime/src/lib.rs`).

## Boot inputs

|Input|Read by|Effect|
|---|---|---|
|`rdinit=/egyskinit`|Kernel command line|Selects the patched initializer. The archive contains no `/init`.|
|`esp_device` in `/egysk.toml`|`storage::open_early`|Overrides GPT ESP discovery.|
|`[backing]` in `/egysk.toml` and `egysk/egysk.toml`|`storage::open_early`|Selects block or filesystem backing. Package values override seed values.|
|`androidboot.egysk.debug=1`|`bootstrap::debug_boot`|Grants UID 2000 the upstream `Allow` policy row and projects `su`. Only the exact value `1` enables it.|
|`egysk.safe_mode=1`|`storage::open_early`|Disables module activation and discovery.|
|`androidboot.mode=recovery`|`bootstrap::boot_mode`|Selects recovery lifecycle.|
|`androidboot.mode=fastboot` or `fastbootd`|`bootstrap::boot_mode`|Selects recovery lifecycle.|
|`androidboot.recovery=1`|`bootstrap::boot_mode`|Selects recovery lifecycle.|
|`androidboot.mode=charger`|`bootstrap::boot_mode`|Selects charger mode. Modules do not load.|
|`androidboot.force_normal_boot=1`|`bootstrap::boot_mode`|Prevents the `/sbin/recovery` probe from selecting recovery.|
|`androidboot.surfacer.boot_count_path`|`dispatch::bless_boot`|Names the counted boot entry to bless on filesystem backing.|

Bootconfig overrides the kernel command line per key, including explicitly disabled values
(`magisk/runtime/src/lib.rs`).

## Boot sequence

The consumer adds the archive to a ramdisk and passes `rdinit=/egyskinit`. The archive has no
`/init`, so egysk never supplies an initializer from a different ROM.

1. `egyskinit` mounts a nosuid `/dev` tmpfs when `/dev` is not mounted, then creates `null`,
   `kmsg`, `random`, and `urandom` (`magisk/product/init.rs`).
2. It requires `/egysk/modules/lkm-loader/module.prop` and `pid1.sh`. It runs
   `busybox sh pid1.sh` with `ASH_STANDALONE=1` before first-stage mounts. A failure stops the
   boot.
3. `pid1.sh` loads the stock vendor module list with `lkm-loader --vendor`. When
   `kmod/modules.load` exists, it also loads that ordered list with `lkm-loader --dir`.
   The loader resolves by-name parameters, parses the whole list before loading, and stops at the
   first failure (`modules/lkm-loader/pid1.sh`, `modules/lkm-loader/src/main.rs`).
   This step runs on every boot, before egysk classifies the boot mode. Recovery, charger mode,
   `egysk.safe_mode=1`, and the `disable`, `remove`, and `recovery_ok` flags do not apply to it.
   The boot-payload module comes from the archive (`/egysk/modules/lkm-loader`), not from module
   discovery.
4. `storage::open_early` mounts the ESP, merges seed and package configuration, classifies the boot
   mode, stages tools into `/dev/egysk/bin`, and prepares backing storage.
5. With block backing, it reclaims lower caches, activates a pending package, discovers modules,
   mounts read-only module views, and runs each module's `rdinit` scripts under one 35-second deadline
   that all `rdinit` phases share (`magisk/product/init.rs`).
6. With filesystem backing, no store, view, or `rdinit` hook exists yet. Only tool staging and
   package RC/policy admission happen here. The stock initializer owns the backing mount.
7. `storage::save` keeps boot inputs, tools, raw module RC, module `sepolicy.rule` content, and
   `module-policy` in Magisk's RAM overlay. There is no encoded inventory, stage progress, or
   replayable service schedule.
8. Magisk backs up the stock initializer to `/.egysk-backup` and writes `RECOVERYMODE=false`.
   That key is upstream's recovery-as-boot installation flag, not the current boot mode. The
   bootloader already selected the ramdisk.
9. After stock early-init, coldboot, and bootstrap APEX setup, `egyskd` rebuilds storage at the
   front of the init event, discovers modules, mounts Android-stage views, stages module files,
   and records prepared markers (`magisk/runtime/src/storage.rs`).
10. Magisk runs the remaining module stages through its own serialized stage runner. Egysk adds
    phase suffixes through `BootstrapConfig::phases` (`magisk/product/dispatch.rs`).

### Recovery

Recovery keeps the upstream SELinux and rootdir interception path. It intercepts the original
initializer's direct second-stage exec instead of a switch-root mount. The target is `/init.real`
when that file exists, otherwise `/init` (`magisk/patches/Magisk/0004-native-init-and-early-hooks.patch`).
Module RC and admitted module stages also precede stock actions on each event.

Module discovery in recovery admits an ESP or local module only when its `recovery_ok` flag is set
(`magisk/runtime/src/store.rs`). This filter does not apply to the PID 1 boot-payload LKMs in step 3.
Those LKMs load before the boot mode is known. Charger mode bypasses interception and discovers no
modules.

## Storage layout

|Path|Content|
|---|---|
|`/dev/esp`|The selected ESP, mounted `nosuid,nodev,noexec`.|
|`/dev/esp/egysk/`|Shared package: config, binaries, modules, release sets.|
|`/dev/esp/rom/<id>/`|Per-ROM records owned by the platform packager.|
|`/dev/egysk/bin/`|Executable tmpfs tools staged from the package or `/egysk/bin`.|
|`/dev/egysk/store/`|Shared-store subtree. It holds local modules, overlays, state, preferences, and logs.|
|`/dev/egysk/lowers/`|Read-only boot-local module baselines copied from the ESP.|
|`/dev/egysk/modules/<id>/`|Effective writable module views for this boot.|
|`/dev/egysk/state/<id>/`|Durable module state bind for this boot.|
|`/dev/egysk/devices/`|Created device nodes for ESP discovery.|
|`/egysk.toml`|Seed configuration in the appended archive.|
|`/dev/esp/egysk/egysk.toml`|Package configuration on the ESP.|

Constants live in `magisk/runtime/src/context.rs`.

## Backing configuration

Filesystem backing adopts a mount that stock init already created. It creates only the store bind.

```toml
[backing]
kind = "filesystem"
source = "/metadata"
fs_type = "f2fs"
mount_at = "/metadata"
```

Block backing calls the named helper. The helper selects the store volume through its arguments,
because the core checks `source` before the helper runs.

```toml
[backing]
kind = "block"
fs_type = "ext4"
mount_at = "/dev/egysk/metadata"
helper = "maos-credentials"
args = ["--lv", "rom/store"]
```

The helper contract is `prepare-backing` with literal arguments and no shell. It writes JSON
`{"owned_mounts": [...]}` to stdout, and the core accepts only the store or the configured mount
point. The helper owns the volume and never formats it (`magisk/runtime/src/bootstrap.rs`). The helper has a 35-second deadline.

Filesystem adoption requires a mounted source that equals `mount_at` with the configured
filesystem type. Native adoption fails instead of using an unmounted directory. The private store
is `<mount_at>/egysk`, not the whole filesystem.

## Module inventory and views

Modules come from two owners. ESP modules live under `/dev/esp/egysk/modules`, and local modules
live under `/dev/egysk/store/modules`. A module ID may belong to only one owner. Each directory
must contain a `module.prop` whose `id` matches the directory name.

`modules_order` in the package sets precedence. Unlisted modules follow in sorted order. A module
is skipped when its `disable` or `remove` flag is set, or when recovery lacks `recovery_ok`.
Preference files under `/dev/egysk/store/preferences/<id>/` override package flags when the store
is available (`magisk/runtime/src/store.rs`).

The ESP is VFAT, so its module content is copied to read-only tmpfs lowers under
`/dev/egysk/lowers`. Generation identity binds upper/work pairs to package bytes. The overlay
mount uses `index=off,metacopy=on,xino=off,redirect_dir=on,nfs_export=off`. Boot fails when
metacopy is off, because a silent downgrade would copy unchanged data into the store.

Durable module state is outside generations. Activation requires all effective views to be
unmounted. A synced journal precedes each multi-directory transaction, and recovery uses content
identity rather than directory presence.

## SELinux and authorization

Magisk's native policy builder owns the `egysk` daemon domain, the `egysk_file` type, and the
`egysk_log_file` type. The daemon domain inherits upstream Magisk's broad permissions and its
per-domain permissive setting. There is no second bootstrap policy pass.

The egysk policy delta covers early tmpfs labels, backing labels, a private efivarfs association,
nosuid RC service transitions, and the fatal early-stage path
(`magisk/patches/Magisk/0003-policy-and-dependency-errors.patch`).

Module `sepolicy.rule` content still governs module services. This does not make the boot HAL or
Gatekeeper domains permissive.

`su` authorization stays with upstream Magisk. `egyskd` reconciles
the UID 2000 row on every start, so a previous debug boot cannot leave a grant behind
(`magisk/runtime/src/lib.rs`, `magisk/product/dispatch.rs`).

## Logging and failure

The daemon opens `/dev/egysk/store/egysk.log` when the store is mounted. It rotates the log to
`egysk.log.bak` at daemon start and when an append would exceed 512 KiB. Only one backup is kept.
Warn and Error records are fsynced. Before the store is available, only kmsg is available
(`magisk/runtime/src/bootlog.rs`).

A fatal boot writes the cause to kmsg, the boot log, and `store/last-failure`. When the failing
process is PID 1, egysk exits, which panics the kernel. Other processes request a kernel panic
through `/proc/sysrq-trigger` and then park. Egysk never replaces a failure with a silent reboot
(`magisk/runtime/src/lib.rs`).

Blocking module hooks run under a 35-second stage deadline. The stage runner keeps a bounded
stderr tail and reports the stage, exit status, and tail on failure. Signal exit is reported as
`128 + signal` (`magisk/runtime/src/scripts.rs`).

## Script environment

Module scripts receive `EGYSK_BACKING_ROOT`, `EGYSK_MODULE_STATE` (`/dev/egysk/state/<id>`), and `EGYSK_MODE` (`normal`, `recovery`, or `charger`) (`magisk/runtime/src/scripts.rs`).
Preparation that must precede a consumer belongs in the blocking stage script, not in
an asynchronous property action or a `ctl.start` request.

Phase hooks follow the configured order: `stage.sh` first, then `stage.<phase>.sh` for each phase
in `BootstrapConfig::phases[stage]`.

## Direct ESP package

A direct package holds ESP files only. It has no `rom/` tree, no launcher entry, and no maOS
binary. The packager writes `egysk/build-id`, `egysk/egysk.toml`, `egysk/bin/`, the
`lkm-loader` module, `egysk/modules_order`, and `direct/egysk.cpio`
(`magisk/scripts/package_direct.py`).

Direct mode uses filesystem backing with `source = "/metadata"`. A direct package has no `kmod/`
directory, so `pid1.sh` loads only the stock vendor list. When a package supplies
`kmod/modules.load`, the loader also runs and rejects an empty list.

The archive member `direct/egysk.cpio` uses the LZ4 legacy format with magic `0x184c2102`, despite
the suffix. `egysk-build-cpio` writes that framing through its `--legacy-lz4` option, which is a
published CLI flag used by maOS packaging.

On filesystem backing, `dispatch::bless_boot` renames the counted boot entry named by
`androidboot.surfacer.boot_count_path`. This follows systemd automatic boot assessment. A managed
ROM blesses its own entries, and a blessing failure is logged but is not fatal.

## Boundaries

Egysk does not do the following:

- Install or write packages to a device.
- Produce, replace, or publish GBL protocols.
- Provide a kernel helper, sidecar daemon, or alternate root-provider ABI.
- Ship adb, its authorization, or the USB configuration.
- Own Android keys, credential state, or the maOS partition projection.
- Treat a successful build or archive as an enforcing-boot result.

Platform contracts for projection, credentials, and OTA live in `maos` and `maos-hals`. Kernel
modules and their KMI admission live in `maos-lkms`.
