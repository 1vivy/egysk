// SPDX-License-Identifier: GPL-3.0-only
//! Explicit-input archive construction; never discovers or writes a device.
use android_bootimg::cpio::{Cpio, CpioEntry};
use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use goblin::elf::{Elf, header, program_header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

/// Bootstrap metadata is producer input, not a runtime admission framework.
fn config_member() -> &'static str {
    "egysk.toml"
}
const LOADER: &str = "egyskinit";
const RECEIPT: &str = "egysk-build.json";
const LZ4_BLOCK: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum Arch {
    Aarch64,
    #[value(name = "x86_64")]
    X86_64,
}
impl Arch {
    fn name(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }
    fn machine(self) -> u16 {
        match self {
            Self::Aarch64 => 183,
            Self::X86_64 => 62,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Entry {
    Egyskinit,
}
impl Entry {
    fn name(self) -> &'static str {
        match self {
            Self::Egyskinit => LOADER,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "egysk-build-cpio",
    version,
    about = "Build a standalone rdinit archive from explicit release inputs"
)]
struct Args {
    #[arg(long, required = true)]
    build_cpio: bool,
    #[arg(long)]
    legacy_lz4: bool,
    /// Exact branch-generation, or a branch with exactly one available generation.
    #[arg(long)]
    kmi: String,
    #[arg(long, value_enum)]
    arch: Arch,
    #[arg(long)]
    artifact_dir: PathBuf,
    /// Root-relative configuration, static early tools and the critical LKMs.
    #[arg(long)]
    bootstrap_dir: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, value_enum, default_value = "egyskinit")]
    entry: Entry,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Kmi {
    branch: String,
    generation: u32,
}
impl Kmi {
    fn directory(&self) -> String {
        format!("{}-{}", self.branch, self.generation)
    }
}

struct Input {
    bytes: Vec<u8>,
    mode: u32,
}
#[derive(Serialize)]
struct FileReceipt {
    sha256: String,
    size: usize,
    mode: u32,
}
#[derive(Serialize)]
struct BuildReceipt {
    schema_version: u32,
    kmi: Kmi,
    arch: Arch,
    entry: &'static str,
    files: BTreeMap<String, FileReceipt>,
}

fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 255
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
fn member(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && path.split('/').all(identifier),
        "unsafe archive path: {path}"
    );
    let root = path.split('/').next().unwrap();
    ensure!(
        !matches!(root, "init" | LOADER | RECEIPT),
        "bootstrap payload claims reserved archive entry: {path}"
    );
    ensure!(
        !path.split('/').any(|part| part == "egysk.ko"),
        "obsolete kernel helper is forbidden: {path}"
    );
    ensure!(
        !path.ends_with(".compat.json") && !path.ends_with(".build.json"),
        "producer module metadata is not a payload member: {path}"
    );
    ensure!(
        path != "egysk/bin/magiskinit",
        "duplicate initializer is forbidden"
    );
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

fn read_input(path: &Path) -> Result<Input> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let before = file.metadata()?;
    ensure!(before.is_file(), "not a regular input: {}", path.display());
    ensure!(
        before.len() <= u64::from(u32::MAX),
        "input exceeds newc size: {}",
        path.display()
    );
    let mut bytes = Vec::with_capacity(usize::try_from(before.len())?);
    (&mut file)
        .take(u64::from(u32::MAX) + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    ensure!(
        bytes.len() as u64 == before.len()
            && after.len() == before.len()
            && after.modified()? == before.modified()?,
        "input changed while captured: {}",
        path.display()
    );
    Ok(Input {
        bytes,
        mode: before.permissions().mode() & 0o777,
    })
}

fn capture(
    root: &Path,
    relative: &Path,
    files: &mut BTreeMap<String, Input>,
    dirs: &mut BTreeSet<String>,
) -> Result<()> {
    let path = root.join(relative);
    ensure!(
        fs::symlink_metadata(&path)?.is_dir(),
        "bootstrap directory must not be a symlink: {}",
        path.display()
    );
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let name = child.to_str().context("non-UTF8 bootstrap path")?;
        member(name)?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            dirs.insert(name.to_owned());
            capture(root, &child, files, dirs)?;
        } else {
            ensure!(
                kind.is_file(),
                "bootstrap requires regular files/directories: {name}"
            );
            files.insert(name.to_owned(), read_input(&entry.path())?);
        }
    }
    Ok(())
}

fn split_kmi(value: &str) -> Option<Kmi> {
    let (branch, generation) = value.rsplit_once('-')?;
    let (android, kernel) = branch.split_once('-')?;
    let version = android.strip_prefix("android")?;
    if version.is_empty() || !version.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    let (major, minor) = kernel.split_once('.')?;
    if major.is_empty()
        || minor.is_empty()
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|v| v.is_ascii_digit())
    {
        return None;
    }
    Some(Kmi {
        branch: branch.to_owned(),
        generation: generation.parse().ok()?,
    })
}
fn select(args: &Args) -> Result<(Kmi, PathBuf)> {
    ensure!(identifier(&args.kmi), "invalid KMI selector");
    let mut selected = Vec::new();
    for entry in fs::read_dir(&args.artifact_dir).context("read artifact directory")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(kmi) = split_kmi(name) else {
            continue;
        };
        if name == args.kmi || kmi.branch == args.kmi {
            let directory = entry.path().join(args.arch.name());
            if fs::symlink_metadata(&directory).is_ok_and(|m| m.is_dir()) {
                selected.push((kmi, directory));
            }
        }
    }
    ensure!(
        selected.len() == 1,
        "KMI {} / {} resolves to {} artifact sets; supply one exact generation",
        args.kmi,
        args.arch.name(),
        selected.len()
    );
    Ok(selected.pop().unwrap())
}

fn elf<'a>(bytes: &'a [u8], arch: Arch) -> Result<Elf<'a>> {
    let elf = Elf::parse(bytes).context("parse payload ELF")?;
    ensure!(
        elf.is_64 && elf.little_endian && elf.header.e_machine == arch.machine(),
        "ELF architecture mismatch"
    );
    Ok(elf)
}
fn executable(bytes: &[u8], arch: Arch, static_required: bool) -> Result<()> {
    let elf = elf(bytes, arch)?;
    ensure!(
        matches!(elf.header.e_type, header::ET_EXEC | header::ET_DYN),
        "expected executable ELF"
    );
    ensure!(
        !static_required || (elf.interpreter.is_none() && elf.libraries.is_empty()),
        "early executable must be static"
    );
    let mut entry_backed = false;
    for segment in elf
        .program_headers
        .iter()
        .filter(|s| s.p_type == program_header::PT_LOAD)
    {
        ensure!(
            segment.p_filesz <= segment.p_memsz
                && segment
                    .p_offset
                    .checked_add(segment.p_filesz)
                    .is_some_and(|end| end <= bytes.len() as u64),
            "ELF load segment outside input"
        );
        let end = segment
            .p_vaddr
            .checked_add(segment.p_filesz)
            .context("ELF address overflow")?;
        entry_backed |= segment.p_flags & program_header::PF_X != 0
            && (segment.p_vaddr..end).contains(&elf.entry);
    }
    ensure!(
        entry_backed,
        "ELF entry lacks executable file-backed segment"
    );
    Ok(())
}
fn section<'a>(bytes: &'a [u8], elf: &Elf<'_>, name: &str) -> Result<&'a [u8]> {
    let Some(section) = elf
        .section_headers
        .iter()
        .find(|s| elf.shdr_strtab.get_at(s.sh_name) == Some(name))
    else {
        return Ok(&[]);
    };
    let start = usize::try_from(section.sh_offset)?;
    let end = start
        .checked_add(usize::try_from(section.sh_size)?)
        .context("ELF section overflow")?;
    bytes.get(start..end).context("ELF section outside input")
}
fn verify_module(name: &str, input: &Input, arch: Arch) -> Result<()> {
    let elf = elf(&input.bytes, arch)?;
    ensure!(
        elf.header.e_type == header::ET_REL,
        "kernel module is not ET_REL: {name}"
    );
    let internal = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("invalid module filename")?
        .replace('-', "_");
    let modinfo = section(&input.bytes, &elf, ".modinfo")?;
    let names: Vec<_> = modinfo
        .split(|&v| v == 0)
        .filter_map(|s| s.strip_prefix(b"name="))
        .collect();
    ensure!(
        names.as_slice() == [internal.as_bytes()],
        "module internal name mismatch: {name}"
    );
    let vermagics: Vec<_> = modinfo
        .split(|&v| v == 0)
        .filter_map(|s| s.strip_prefix(b"vermagic="))
        .collect();
    ensure!(vermagics.len() == 1, "missing/duplicate module vermagic");
    let tokens: Vec<_> = std::str::from_utf8(vermagics[0])?
        .split_whitespace()
        .collect();
    let flags: &[&str] = match arch {
        Arch::Aarch64 => &["SMP", "preempt", "mod_unload", "modversions", "aarch64"],
        Arch::X86_64 => &["SMP", "preempt", "mod_unload", "modversions"],
    };
    ensure!(
        tokens.len() == flags.len() + 1 && &tokens[1..] == flags,
        "module vermagic flags mismatch"
    );
    Ok(())
}

fn legacy_lz4(input: &mut File, output: &mut File) -> Result<()> {
    output.write_all(&0x184c_2102u32.to_le_bytes())?;
    let mut chunk = vec![0; LZ4_BLOCK];
    loop {
        let mut used = 0;
        while used < chunk.len() {
            let count = input.read(&mut chunk[used..])?;
            if count == 0 {
                break;
            }
            used += count;
        }
        if used == 0 {
            break;
        }
        let compressed = lz4::block::compress(
            &chunk[..used],
            Some(lz4::block::CompressionMode::HIGHCOMPRESSION(12)),
            false,
        )?;
        output.write_all(&u32::try_from(compressed.len())?.to_le_bytes())?;
        output.write_all(&compressed)?;
    }
    Ok(())
}

fn build(args: &Args) -> Result<()> {
    ensure!(args.build_cpio, "--build-cpio is required");
    ensure!(
        args.out
            .components()
            .all(|part| !matches!(part, Component::ParentDir)),
        "output contains parent traversal"
    );
    let (kmi, artifacts) = select(args)?;
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    capture(&args.bootstrap_dir, Path::new(""), &mut files, &mut dirs)?;
    let config: toml::Value = toml::from_str(std::str::from_utf8(
        &files
            .get(config_member())
            .with_context(|| format!("bootstrap requires {}", config_member()))?
            .bytes,
    )?)?;
    ensure!(
        config.get("kmi").and_then(toml::Value::as_str) == Some(kmi.directory().as_str()),
        "bootstrap config KMI must equal selected exact generation"
    );
    let init = read_input(&artifacts.join(LOADER))?;
    executable(&init.bytes, args.arch, true).context("selected native initializer")?;
    files.insert(
        args.entry.name().to_owned(),
        Input {
            bytes: init.bytes,
            mode: 0o755,
        },
    );
    for name in [
        "egysk/bin/egyskd",
        "egysk/bin/magiskpolicy",
        "egysk/bin/busybox",
        "egysk/modules/lkm-loader/bin/lkm-loader",
    ] {
        let input = files
            .get(name)
            .with_context(|| format!("bootstrap requires {name}"))?;
        executable(
            &input.bytes,
            args.arch,
            !matches!(name, "egysk/bin/egyskd" | "egysk/bin/magiskpolicy"),
        )
        .with_context(|| format!("payload {name}"))?;
        ensure!(input.mode == 0o755, "{name} must be executable");
    }
    for name in ["module.prop", "pid1.sh"] {
        ensure!(
            files.contains_key(&format!("egysk/modules/lkm-loader/{name}")),
            "bootstrap requires lkm-loader/{name}"
        );
    }
    let preload = files
        .get("egysk/bin/init-ld")
        .context("bootstrap requires init-ld")?;
    let preload_elf = elf(&preload.bytes, args.arch)?;
    ensure!(
        preload_elf.header.e_type == header::ET_DYN && preload_elf.interpreter.is_none(),
        "init-ld must be a shared ELF without an interpreter"
    );
    for (name, input) in &files {
        if name.ends_with(".ko") {
            verify_module(name, input, args.arch).with_context(|| format!("verify {name}"))?;
        } else if name.ends_with(".so") {
            let library = elf(&input.bytes, args.arch)?;
            ensure!(
                library.header.e_type == header::ET_DYN && library.interpreter.is_none(),
                "module library must be a shared ELF without an interpreter: {name}"
            );
        } else if name != "egysk/bin/init-ld" && input.bytes.starts_with(b"\x7fELF") {
            executable(
                &input.bytes,
                args.arch,
                !matches!(name.as_str(), "egysk/bin/egyskd" | "egysk/bin/magiskpolicy"),
            )
            .with_context(|| format!("payload {name}"))?;
        }
    }
    let receipt = BuildReceipt {
        schema_version: 1,
        kmi,
        arch: args.arch,
        entry: args.entry.name(),
        files: files
            .iter()
            .map(|(name, input)| {
                (
                    name.clone(),
                    FileReceipt {
                        sha256: digest(&input.bytes),
                        size: input.bytes.len(),
                        mode: input.mode,
                    },
                )
            })
            .collect(),
    };
    let mut cpio = Cpio::new();
    for directory in dirs {
        cpio.add(&directory, CpioEntry::dir(0o755))?;
    }
    for (name, input) in files {
        cpio.add(&name, CpioEntry::regular(input.mode, Box::new(input.bytes)))?;
    }
    cpio.add(
        RECEIPT,
        CpioEntry::regular(0o644, Box::new(serde_json::to_vec_pretty(&receipt)?)),
    )?;
    let parent = args
        .out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut raw = tempfile::tempfile_in(parent)?;
    cpio.dump(&mut raw)?;
    let length = raw.stream_position()?;
    let padding = (512 - length % 512) % 512;
    raw.write_all(&[0; 512][..usize::try_from(padding)?])?;
    raw.seek(SeekFrom::Start(0))?;
    let mut output = tempfile::NamedTempFile::new_in(parent)?;
    if args.legacy_lz4 {
        legacy_lz4(&mut raw, output.as_file_mut())?;
    } else {
        std::io::copy(&mut raw, output.as_file_mut())?;
    }
    output
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    output.as_file().sync_all()?;
    output
        .persist_noclobber(&args.out)
        .map_err(|error| error.error)
        .context("publish archive without replacing existing output")?;
    File::open(parent)?.sync_all()?;
    println!("{}", args.out.display());
    Ok(())
}
fn main() -> Result<()> {
    build(&Args::parse())
}

#[cfg(test)]
mod tests;
