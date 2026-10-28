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
The EFI names the boot-control owner in the bootconfig key `androidboot.maos.boot_control`. Egysk
admits a module that contains a `virtual_only` file only when that value is exactly `virtual`. An
absent or unknown value admits nothing, so a missing key cannot install a replacement HAL, its
policy, or its RC. Egysk does not track or record the owner, and it does not count or bless boot
entries. It reads the key on each boot.

Evidence: `boot_control`, `virtual_only_skipped`, and `virtual_only_skipped_for` in
`magisk/runtime/src/store.rs`, the admission check in `storage::save`
(`magisk/runtime/src/storage.rs`), and the test
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
  boot-mode classification, virtual-only admission, storage contracts, and log rotation. See
  `tools/egysk-tools/src/tests.rs`, `magisk/runtime/tests/boot_contract.rs`, and the test modules
  in `magisk/runtime/src`.
- A successful build, a passing host test, or a successful archive admission shows that the
  exercised inputs satisfy the checked conditions. It does not show that Android boots.
- Enforcing operation, credential enrollment and unlock, sealed projection, fastbootd, recovery boot, and OTA behavior need consumer evidence.
  Each also needs evidence from the platform owner.
- This repository holds no device qualification records. Report device status only from the
  platform owner's records, and report emulator exercise separately from physical-device
  qualification.
- Exact-KMI admission happens at install or OTA time in `maos-lkms`. Egysk does not recheck a
  module against the shared package's initial KMI on every boot.
