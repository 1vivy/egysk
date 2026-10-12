//! Persistent native logging; no pre-mount buffer and one bounded backup.
use crate::context::STORE;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

const LIMIT: u64 = 512 * 1024;
const LOG: &str = "/dev/egysk/store/egysk.log";
const BACKUP: &str = "/dev/egysk/store/egysk.log.bak";
static WRITER: Mutex<()> = Mutex::new(());
static DAEMON_PID: AtomicI32 = AtomicI32::new(0);

pub fn store_mounted() -> bool {
    fs::metadata(STORE)
        .and_then(
            |store| Ok(store.dev() != fs::metadata(Path::new(STORE).parent().unwrap())?.dev()),
        )
        .unwrap_or(false)
}

fn rotate(log: &Path, backup: &Path) -> io::Result<File> {
    match fs::rename(log, backup) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Bound files left by an older, unbounded producer, even if only .bak exists.
    match OpenOptions::new().write(true).open(backup) {
        Ok(file) => file.set_len(file.metadata()?.len().min(LIMIT))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let file = OpenOptions::new().create(true).append(true).open(log)?;
    file.sync_all()?;
    File::open(log.parent().unwrap())?.sync_all()?;
    Ok(file)
}

fn with_log<T>(action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    // Serialize the daemon's writer and fatal threads. Forked native workers
    // must never wait on an inherited mutex whose owner stayed in the parent.
    let _guard = if DAEMON_PID.load(Ordering::Relaxed) == unsafe { libc::getpid() } {
        Some(
            WRITER
                .lock()
                .map_err(|_| io::Error::other("poisoned log lock"))?,
        )
    } else {
        None
    };
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open("/dev/egysk/store/.egysk-log.lock")?;
    // POSIX record locks are not inherited across fork (unlike flock/OFD locks).
    // Closing this descriptor releases the lock, including on any error path.
    let mut flock: libc::flock = unsafe { std::mem::zeroed() };
    flock.l_type = libc::F_WRLCK as _;
    flock.l_whence = libc::SEEK_SET as _;
    loop {
        if unsafe { libc::fcntl(lock.as_raw_fd(), libc::F_SETLKW, &flock) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    action()
}

/// Called once storage is mounted, before the daemon accepts any requests.
pub fn start_daemon() -> io::Result<()> {
    if !store_mounted() {
        return Err(io::Error::other("egysk store is not mounted"));
    }
    DAEMON_PID.store(unsafe { libc::getpid() }, Ordering::Relaxed);
    with_log(|| rotate(Path::new(LOG), Path::new(BACKUP)).map(|_| ()))
}

fn append(log: &Path, backup: &Path, parts: &[&[u8]], sync: bool) -> io::Result<()> {
    let len: usize = parts.iter().map(|part| part.len()).sum();
    if len as u64 > LIMIT {
        return Err(io::Error::other("egysk log record exceeds file bound"));
    }
    // Reopen under the lock: a fatal worker in another process may have rotated.
    let mut file = OpenOptions::new().create(true).append(true).open(log)?;
    if file.metadata()?.len() + len as u64 > LIMIT {
        file.sync_all()?;
        file = rotate(log, backup)?;
    }
    for part in parts {
        file.write_all(part)?;
    }
    if sync {
        file.sync_all()?;
    }
    Ok(())
}

/// Write through, synchronizing warnings/errors and fatal records before return.
pub fn write(parts: &[&[u8]], sync: bool) -> io::Result<()> {
    if !store_mounted() {
        return Ok(());
    }
    with_log(|| append(Path::new(LOG), Path::new(BACKUP), parts, sync))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_and_size_rotation_keep_one_bounded_generation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let log = root.join("egysk.log");
        let backup = root.join("egysk.log.bak");
        fs::write(&log, b"previous boot\n").unwrap();
        rotate(&log, &backup).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"previous boot\n");
        let record = [b'x'; 4096];
        for _ in 0..LIMIT / record.len() as u64 {
            append(&log, &backup, &[&record], false).unwrap();
        }
        append(&log, &backup, &[b"fatal: ", b"hook stderr\n"], true).unwrap();
        assert_eq!(fs::metadata(&backup).unwrap().len(), LIMIT);
        assert_eq!(fs::read(&log).unwrap(), b"fatal: hook stderr\n");
        rotate(&log, &backup).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"fatal: hook stderr\n");
        assert_eq!(fs::read_dir(root).unwrap().count(), 2);
    }
}
