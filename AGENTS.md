# Egysk agent guide

This file extends `~/.agents/AGENTS.md`. It wins within this repository.

## Required skills

- Read and apply `asd-ste100` before you write or change docs, commit messages or code comments.
- Read and apply `engineering-evidence` before you investigate a bug, interpret a probe or state a behavior claim.

## Read first

- `docs/architecture.md`: the runtime contract (boot sequence, backing, modules, policy, failure).
- `docs/building.md`: pinned inputs, build recipes and checks.
- `docs/design.md`: design rationale and evidence boundaries.
- `SECURITY.md`: the trust boundary and disclosure route.

## Boundaries

- Magisk owns the lifecycle. Put product logic in `magisk/product` and `magisk/runtime`. Patch hunks in `magisk/patches/Magisk/series` only expose integration points. The series applies with no fuzz and no offset.
- Change the pins (Magisk `PIN`, ONDK, BusyBox, NDK r29) only with the owner's go-ahead, and update `docs/design.md` in the same change.
- `--legacy-lz4` and the archive magic `0x184c2102` are a published interface that maOS packaging uses. Do not rename or remove them.
- Platform modules, projection, OTA and AVB belong to `maos`. The credential runtime and boot HAL belong to `maos-hals`. Kernel modules and KMI admission belong to `maos-lkms`. Do not change those repositories from here.
- Never write to a phone or a real block device. A build or a host test is not a boot result.
- Keep KernelSU history, copyright notices and upstream license files.

## Checks

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked`
- `python3 magisk/scripts/magisk.py materialize --check`

## Docs

Docs state the current implementation and cite source paths. Do not write change histories, dated status or run logs in docs or comments.

## Commits

Sign commits with the agent key from `~/.gitconfig`. Never use the owner key `616B6A6050DF1E4B`. Push only when the owner tells you to.
