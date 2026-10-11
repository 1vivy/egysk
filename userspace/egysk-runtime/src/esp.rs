//! GPT selection only; module loading belongs to the boot-payload module.
use crate::{context::*, fsutil as fsu, gpt};
use anyhow::{Context, Result, ensure};
use std::fs::{self, File};
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

pub(crate) fn node(path: &Path, device: u64) -> Result<()> {
    fsu::directory(path.parent().context("device parent")?)?;
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    if path.exists() {
        let meta = fs::metadata(path)?;
        ensure!(
            meta.file_type().is_block_device() && meta.rdev() == device,
            "device collision {}",
            path.display()
        );
    } else {
        ensure!(
            unsafe { libc::mknod(name.as_ptr(), libc::S_IFBLK | 0o600, device) } == 0,
            "create {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn dev(path: &Path) -> Result<u64> {
    let value = fs::read_to_string(path)?;
    let (major, minor) = value
        .trim()
        .split_once(':')
        .context("invalid sysfs device")?;
    Ok(libc::makedev(major.parse()?, minor.parse()?))
}

pub(crate) fn mount(device: u64) -> Result<()> {
    node(Path::new(SELECTED_ESP), device)?;
    fsu::mount(
        SELECTED_ESP,
        Path::new(ESP),
        ESP_MOUNT.fs_type,
        ESP_MOUNT.flags,
        ESP_MOUNT.data,
    )
}

pub(crate) fn select(explicit: Option<&str>) -> Result<u64> {
    ensure!(!fsu::mounted(Path::new(ESP))?, "ESP already mounted");
    if let Some(source) = explicit {
        let meta = fs::metadata(source)?;
        ensure!(
            meta.file_type().is_block_device(),
            "ESP source is not a block device"
        );
        mount(meta.rdev())?;
        ensure!(
            Path::new(PACKAGE_CONFIG).is_file(),
            "ESP package config missing"
        );
        return Ok(meta.rdev());
    }
    let mut selected = None;
    for disk in fsu::children(Path::new("/sys/block"))? {
        let sector: u64 = fs::read_to_string(disk.join("queue/logical_block_size"))?
            .trim()
            .parse()?;
        ensure!(
            sector >= 512 && sector.is_power_of_two(),
            "invalid sector size"
        );
        let device = dev(&disk.join("dev"))?;
        let disk_node = PathBuf::from(DEVICE_DIR).join(format!(
            "{}-{}",
            libc::major(device),
            libc::minor(device)
        ));
        node(&disk_node, device)?;
        let file = File::open(disk_node)?;
        let mut header = [0u8; 512];
        // Non-GPT disks (including empty loop/zram devices) are not candidates.
        if file.read_exact_at(&mut header, sector).is_err() {
            continue;
        }
        let Some(header) = gpt::parse_header(&header) else {
            continue;
        };
        let bytes = u64::from(header.entry_count) * u64::from(header.entry_size);
        ensure!(bytes <= 1 << 20, "GPT table exceeds bound");
        let mut table = vec![0; bytes as usize];
        file.read_exact_at(
            &mut table,
            header
                .entries_lba
                .checked_mul(sector)
                .context("GPT offset overflow")?,
        )?;
        for entry in gpt::find_entries(&table, header.entry_size as usize, &gpt::ESP_TYPE_GUID) {
            let start = entry
                .first_lba
                .checked_mul(sector)
                .context("partition offset overflow")?
                / 512;
            for partition in fsu::children(&disk)? {
                if !partition.join("partition").exists() {
                    continue;
                }
                let actual: u64 = fs::read_to_string(partition.join("start"))?
                    .trim()
                    .parse()?;
                if actual != start {
                    continue;
                }
                let candidate = dev(&partition.join("dev"))?;
                let probe = PathBuf::from(DEVICE_DIR).join("probe");
                if probe.exists() {
                    fs::remove_file(&probe)?;
                }
                node(&probe, candidate)?;
                if let Err(error) = fsu::mount(
                    probe.to_str().context("probe path")?,
                    Path::new(ESP),
                    ESP_MOUNT.fs_type,
                    ESP_MOUNT.flags | libc::MS_RDONLY,
                    ESP_MOUNT.data,
                ) {
                    // An EFI partition type does not guarantee a FAT filesystem.
                    // Ignore non-FAT candidates, but retain I/O and access failures.
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|cause| cause.raw_os_error() == Some(libc::EINVAL))
                    {
                        continue;
                    }
                    return Err(error);
                }
                let found = Path::new(PACKAGE_CONFIG).is_file();
                fsu::unmount(Path::new(ESP))?;
                if found {
                    ensure!(selected.is_none(), "ambiguous product ESP");
                    selected = Some(candidate);
                }
            }
        }
    }
    let selected = selected.context("no GPT ESP contains egysk/egysk.toml")?;
    mount(selected)?;
    Ok(selected)
}
