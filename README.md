# Egysk

**Status (2026-10-12)** — Egysk is patched Magisk for early, ESP-backed modules
on multi-OS Android. The native-lifecycle rework replaces the separate Rust PID1
manager and kernel helper. Both native architectures build and archives pass
admission. Payload `1b47402f1ff6` boots the phone permissively with native early
and ordinary module stages, retained magic-mount exports and the boot HAL.
`20261011T064905Z-phone-rdinit-proof` passes on the boot captured by
`20261011T064735Z-phone-loop`; the initial loop's unrelated transient APEX
mapper race was corrected in the read-only proof. ROM1 PIN enrollment and
post-reboot CE unlock pass `20261011T070142Z-credential-enroll`. Cross-ROM
credentials and integrated OTA remain separate qualifications;
see `gobbl-lab/docs/device-results.md`.
The restored upstream policy foundation and generic Gatekeeper passed enforcing
ROM1 boot and existing-PIN unlock in `20261011T165211Z-phone-gatekeeper-live`.
ROM2 recovery completes native module stages with root ADB, enforcing SELinux,
sealed projection and its own logical slot. The managed AIDL HAL starts through
the ordinary post-fs action in `20261011T194912Z-phone-rom2-install`.
The bounded diagnostic archive was replaced by the untimed producer archive.
ROM2 normal boot, fastbootd, cross-ROM credentials and OTA remain separate proofs.

## Ownership

- This repository owns pinned Magisk, narrow upstream integration patches,
  owned native adapters, the ESP/Unix storage adapter and `egysk-build-cpio`.
- `maos` owns conventional platform modules, including partition projection,
  OTA and AVB. The credential runtime and the boot-control HAL live in
  `maos-hals`; `modules/lkm-loader` is owned here with the other native logic.
- `maos-lkms` owns independent thin, partition-projection, efivarfs and
  efivar_store modules and their exact-KMI producers.

`egyskinit` is the patched **Magiskinit** binary, not another initializer.
`egyskd` is the patched **Magisk daemon**, not a sidecar daemon. There is no
`egysk.ko`, helper ioctl ABI, kernel RC injector or separate lifecycle manager.
Magisk owns initialization, SELinux, init RC, module inventory, serialized stage
execution and magic mount. The Rust `egysk-runtime` crate is only the linked
ESP/Unix storage adapter; it is not a process or stage manager.

The core origin is `https://github.com/1vivy/egysk.git`; private sibling origins
are `1vivy/maos` and `1vivy/maos-lkms`. Product paths, sockets, services
and SELinux identities are distinct from ordinary user root installations.
Egysk does not claim their `su` entrypoint, forward SU requests, negotiate root
providers or ship a Manager. Retained upstream authorization is not stripped
merely because app SU is not the product's purpose.

SELinux uses Magisk's built-in foundation under `egysk`, `egysk_file` and
`egysk_log_file`, including its broad daemon permissions, ioctl access and
permissive daemon domain. There is no replacement operation allowlist or second
bootstrap policy pass. The small integration delta covers early tmpfs/backing
labels, private efivarfs association, nosuid RC transitions and fatal init handling.
Other root installations retain their policy loaders and `/data/adb` access.
Module `sepolicy.rule` files still own service-specific policy; this does not make
the boot HAL or Gatekeeper domains permissive. SU authorization is unchanged;
there is no ESP ACL implementation.

## Boot lifecycle

The consumer selects `rdinit=/egyskinit`. The archive deliberately has no
`/init`: Magiskinit uses the current boot's stock initializer through its normal
RAM backup/restore and handoff, never a bundled initializer from another ROM.

The bootloader selects the normal or recovery ramdisk. Egysk therefore leaves
upstream's recovery-as-boot installation flag `RECOVERYMODE=false`; setting it
from the current recovery mode would select legacy system-as-root and can wait
forever for a system partition under `rootwait`. Relative `/init` symlinks are
resolved before backup so moving the backup cannot change their target.
Managed recovery retains the same upstream SELinux/rootdir interception and
admitted module lifecycle. Two-stage recovery uses the original init's direct
second-stage exec rather than the normal Android switch-root mount trick;
interception targets `/init.real` when present, matching upstream's two-stage
detection rather than patching an outer init wrapper. Otherwise it targets `/init`;
legacy recovery uses the rootfs path. Ramdisk rootfs modifies RC files directly;
read-only roots use bind overlays. Egysk does not mount normal Android's system
root to reach recovery. Charger mode still bypasses interception.
The native projector accepts recovery's ramdisk `/system` using the retained
boot mode; normal Android still requires its mounted filesystem before early
module projection.

1. Magiskinit invokes the boot-payload `lkm-loader` module's `pid1.sh` before
   first-stage mounts. The module loads stock vendor dependencies, then its
   explicitly ordered product LKMs.
2. The storage adapter exposes the raw ESP, physical Unix backing and effective
   module views. Module `rdinit.sh` and `rdinit.<phase>.sh` hooks prepare platform
   state before first-stage init; phases are outermost, modules innermost.
3. Magiskinit integrates module policy and raw module RC through its upstream
   SELinux/rootdir path. Early views are released before handoff.
4. After stock early-init, coldboot and bootstrap APEX setup, the Magisk daemon
   reconstructs storage at the front of the init event, including recovery.
   Native blocking callbacks run early module stages before the corresponding
   stock consumers. In recovery, native callbacks are registered first, then
   admitted module RC, then stock RC, so module actions also precede stock actions
   on each event. Normal Android retains its existing module RC ordering.
5. Upstream module collection, stage serialization, magic mount and later
   post-fs-data/service/boot-completed work remain the single lifecycle.

The new early hooks do not run post-fs-data scripts before `/data` exists.
Module RC is ordinary Android RC, not input to a second parser or service
manager. Platform modules own their domains and service schedules. Preparation
that must precede a consumer belongs in the blocking stage script, not an
asynchronous property action or a `ctl.start` request. Scripts must not wait for
later init events, CE unlock or directories created by later stock actions.

`lkm-loader` is a static module tool: `--vendor` loads the stock vendor list;
`lkm-loader MODULE [PARAMS]` provides assisted insmod. Exact-KMI compatibility is
admitted before installation or OTA, not rechecked against the shared package's
initial KMI on every boot. Native core binaries are kernel-independent.

Fatal product failures must remain real panics with a logged cause, not quiet
reboots. Diagnostic Android boots also use
`androidboot.init_fatal_panic=true` for init's own fatal failures. Permissive boot
and AVC review are distinct from enforcing qualification.

## ESP and Unix backing

```text
/dev/esp                         one raw RW ESP, nosuid,nodev,noexec
/dev/esp/egysk/                  shared config, binaries, modules, release sets
/dev/esp/rom/<id>/                per-ROM records: rom.toml, payloads and images
/dev/egysk/bin/                  executable tmpfs tools
/dev/egysk/store/                physical-metadata product subtree
/dev/egysk/lowers/               read-only boot-local Unix module baselines
/dev/egysk/modules/<id>/         effective writable module views
/dev/egysk/state/<id>/           shared durable module state
```

The backing descriptor is opaque to the core. A separately staged helper exposes
the existing Unix-capable filesystem; it cannot depend on the views it is about
to construct. There is no module-storage LV, formatting, per-ROM module profile,
capacity manager or silent ephemeral fallback. Android credential state remains
outside module upper/work trees.

VFAT cannot be an OverlayFS lower. ESP module content is copied to verified,
read-only tmpfs lowers; only generation-specific upper/work pairs persist on
physical metadata. `metacopy=on`, `redirect_dir=on`, `index=off`,
`nfs_export=off`, `xino=off` keep chmod/chown/labels from copying unchanged data.
Actual content writes still copy data up. Unsupported metacopy fails rather than
silently filling metadata. This is shared filesystem backing, not a per-ROM
cache or a receipt store.

Generation identity binds deltas to package bytes, not recreated tmpfs inode
numbers. Unchanged generations retain their edits; package changes use distinct
upper/work pairs. Durable module state is outside generations. Never change an
active lower or reuse an upper/work pair while overlays reference it. Package
activation journals recover interrupted renames; they are not boot receipts.
Module removal must never traverse credential siblings.

## Native build and upstream rebases

Magisk is pinned at `e8915d9db15f5aae93973ffe65068e34df375a6a` in
`magisk/vendor/Magisk`. `magisk/patches/Magisk/series` is the modification authority.
`magisk/scripts/magisk.py` rejects patch fuzz, offsets and source drift and materializes
an `out/<tree>` build. Product logic lives in owned sources; patch hunks expose
integration points rather than growing another implementation inside upstream.
`magisk/product/identity.json` supplies native identity constants.

```sh
git submodule update --init --recursive
python3 magisk/scripts/magisk.py --output out/Magisk build \
  --ndk /path/to/ondk-r30.1 --static-ndk /path/to/android-ndk-r29 --abi arm64-v8a
```

The producer uses ONDK's compiler/Rust toolchain and locked dependencies.
Magiskinit links full static Bionic from NDK r29: early filesystem/process hooks
exceed crt0's small libc subset, and r30 libc embeds conflicting Rust std symbols.
`out/native/<abi>/bin` supplies `egyskinit`, `egyskd`, `magiskpolicy`,
`init-ld` and installer utilities. Static BusyBox is a separate explicit input.
The same core binaries can be packaged with independently admitted LKM sets for
multiple KMIs. Kernel source, DDK and module build recipes live in `maos-lkms`,
not in the core build.

## Host archive builder

```sh
cargo build --release --locked -p egysk-tools
target/release/egysk-build-cpio --build-cpio --legacy-lz4 \
  --kmi android16-6.12-6 --arch aarch64 --artifact-dir /path/to/releases \
  --bootstrap-dir /path/to/bootstrap --entry egyskinit \
  --out /path/to/egysk.cpio.lz4
```

The builder consumes explicit inputs and downloads nothing. Bootstrap supplies
`egysk.toml`, native companions, the module objects with their producer
compatibility receipts, and independent early tools. It never supplies stock
`/init`, a second Magiskinit or the retired kernel helper. The owned PID 1
loader module lives in `modules/lkm-loader`: `pid1.sh` loads the stock vendor
modules, then the ordered `kmod/modules.load` payload list the platform packager
writes from the seed's critical modules. A branch-only KMI must resolve to
exactly one generation for the architecture.

`egysk-build.json` inventories architecture, exact KMI, entry and member
hashes/sizes/modes. It excludes itself and has no package build ID. The platform
packager owns the separate immutable product `build-id` and schema-2 `set.json`.
These are producer/install/OTA admission data, not runtime lifecycle receipts.
`rom/<id>/egysk.cpio`, `egysk.stage.cpio` and the EFI wire bytes stay unchanged.

## Checks and qualification boundaries

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --offline
```

Build both native Android architectures and exercise the produced archive and
real device lifecycle. Host tests and successful linking do not prove a boot.
Historical permissive evidence `20261010T232708Z-egysk-cutover` belongs to the
removed helper/runtime implementation, not the replacement.

Platform obligations are unchanged: peer installation identity and GSI/DSU
credential ownership; no automatic namespace adoption or recycling; stock
Gatekeeper scheduling; independent Partformer ABI6 and LVM ownership; native
EFI semantics and exact partition identity. See `maos` for those
contracts. The planning assumption remains 16 Weaver slots for five Android
installations, roughly three each—not measured capacity or a quota. Organic
exhaustion remains Android/backend behavior and must not be masked.

Enforcing operation, credential enrollment/PIN verification, anchored
seal/reload/busy teardown and integrated OTA/rollback/HAL I/O require their own
observations and exact authorization. A boot does not prove those operations.
No successful check authorizes push, deployment, repository changes on the
server or device writes.

KernelSU history and copyright notices are retained. [LICENSE](LICENSE) remains
GPL-3.0; pinned Magisk and other subtrees retain their licenses. See
[SECURITY.md](SECURITY.md) for the product boundary and disclosure route.
