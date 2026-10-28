# Egysk

Egysk patches Magisk for early, ESP-backed modules on Android. It provides a native initializer, daemon, storage adapter, and archive builder.

## Ownership

- Egysk owns the pinned Magisk integration, native adapters, ESP/Unix storage adapter, `egysk-build-cpio`, and `modules/lkm-loader`.
- `maos` owns platform modules, partition projection, OTA, and AVB integration. maOS consumes specification-compliant GBL EFI protocols from independent producers.
- `maos-hals` owns the credential runtime and boot-control HAL.
- `maos-lkms` owns independent kernel modules and their exact-KMI producers.

`egyskinit` is patched Magiskinit. `egyskd` is the patched Magisk daemon. Magisk owns initialization, SELinux integration, module stages, and magic mount.
The linked Rust `egysk-runtime` crate adapts storage. It is not a separate process or lifecycle manager.

Egysk produces files for platform packagers. It does not install firmware, produce GBL protocols, or write packages to devices.
The core repository is [1vivy/egysk](https://github.com/1vivy/egysk).

## Documentation

- [Architecture and runtime contract](docs/architecture.md): lifecycle, backing storage, module ownership, policy, and failure handling.
- [Build and contributor procedures](docs/building.md): pinned inputs, direct packages, archive production, and checks.
- [Design rationale and evidence](docs/design.md): source provenance, toolchain decisions, and qualification limits.
- [Security policy](SECURITY.md): trust boundaries and private disclosure.
- [Agent instructions](AGENTS.md): contributor boundaries and required shared skills.

## Build a direct package

Use a Linux host with Rust, Python 3, Git, `just`, and Android NDK r29.
Read the [complete prerequisites](docs/building.md) before you start.

```sh
git submodule update --init --recursive
just bootstrap
export ANDROID_NDK_HOME=/path/to/toolchains/android-ndk-r29
just build_direct android16-6.12-6 out/direct-esp
```

Use fresh generated-source and package paths. The recipe refuses to replace an existing package.
It creates ESP files, not a launcher entry or a device installation.
The package uses the LZ4 legacy format (magic `0x184c2102`), despite the `direct/egysk.cpio` suffix.

## Qualification

A successful build or host test does not establish Android boot, enforcing operation, credentials, recovery, fastbootd, or OTA behavior.
Each operation requires evidence from its actual consumer. Physical-device qualification remains separate from host and emulator checks.

KernelSU history and copyright notices retain their legal meaning. [LICENSE](LICENSE) is GPL-3.0.
Pinned Magisk and independently licensed subtrees retain their own licenses.
