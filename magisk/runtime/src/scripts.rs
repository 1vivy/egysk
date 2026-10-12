//! Bounded process-group script execution shared by rdinit and the native daemon.
use crate::context::*;
use anyhow::{Context, Result, bail, ensure};
use std::collections::VecDeque;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Native blocks signals before worker creation; never pass that mask to scripts.
pub fn child_signals(command: &mut Command) {
    // SAFETY: this pre-exec callback uses only async-signal-safe libc operations.
    unsafe {
        command.pre_exec(|| {
            let mut empty = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(empty.as_mut_ptr());
            if libc::sigprocmask(libc::SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn drain_stderr(child: &mut Child, tail: &mut VecDeque<u8>) -> std::io::Result<()> {
    let Some(stderr) = child.stderr.as_mut() else {
        return Ok(());
    };
    let mut bytes = [0u8; 512];
    // Bound each drain so continuous output cannot starve the stage deadline.
    for _ in 0..128 {
        let mut poll = libc::pollfd {
            fd: stderr.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one live entry; timeout zero never blocks.
        let ready = unsafe { libc::poll(&mut poll, 1, 0) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            break;
        }
        if poll.revents & libc::POLLNVAL != 0 {
            return Err(std::io::Error::from_raw_os_error(libc::EBADF));
        }
        if poll.revents & (libc::POLLIN | libc::POLLHUP) == 0 {
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
        // This is the only reader. Readiness (including EOF) cannot be consumed
        // by another reader between poll and read.
        let count = match stderr.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let discard = (tail.len() + count).saturating_sub(bytes.len());
        drop(tail.drain(..discard));
        tail.extend(&bytes[..count]);
    }
    Ok(())
}

pub fn wait_until(child: &mut Child, deadline: Instant, kill_descendants: bool) -> Result<()> {
    let mut stderr = VecDeque::new();
    loop {
        match drain_stderr(child, &mut stderr).and_then(|()| child.try_wait()) {
            Ok(Some(status)) => {
                if kill_descendants {
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                drain_stderr(child, &mut stderr).context("read script failure output")?;
                ensure!(
                    status.success(),
                    "script exited {status}: {}",
                    String::from_utf8_lossy(stderr.make_contiguous()).trim()
                );
                return Ok(());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let pid = i32::try_from(child.id()).context("script PID overflow")?;
                // Own process group; the foreground child has not been reaped.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                child.kill().ok();
                child.wait().context("reap failed/timed-out child")?;
                if let Err(error) = result {
                    return Err(error).context("wait for script");
                }
                bail!(
                    "script exceeded 35-second stage deadline: {}",
                    String::from_utf8_lossy(stderr.make_contiguous()).trim()
                );
            }
        }
    }
}

/// Every phase is a blocking barrier, including late stages. Modules may launch
/// their own daemons, but owning scripts must succeed before RC services start.
pub fn run_one(
    module: &Module,
    directory: &Path,
    script_name: &str,
    mode: BootMode,
    backing_root: &str,
    deadline: Instant,
) -> Result<()> {
    let script = directory.join(format!("{script_name}.sh"));
    match std::fs::symlink_metadata(&script) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("stat module script"),
        Ok(metadata) => ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= 1024 * 1024,
            "invalid module script {}",
            script.display()
        ),
    }
    ensure!(
        Instant::now() < deadline,
        "stage deadline exhausted before {}",
        module.id
    );
    let mut command = Command::new(format!("{BIN}/busybox"));
    child_signals(&mut command);
    let kill_descendants = script_name.starts_with("rdinit") || script_name == "uninstall";
    if kill_descendants {
        // Early hooks have no surviving descendants. Keep their diagnostic tail
        // for fatal_boot without filling metadata or depending on a console.
        // Later hooks retain inherited stderr for daemons that outlive the hook.
        command.stderr(Stdio::piped());
    }
    command
        .arg("sh")
        .arg(&script)
        .current_dir(directory)
        .process_group(0)
        .stdin(Stdio::null())
        .env("ASH_STANDALONE", "1")
        .env("EGYSK", "true")
        .env(
            "EGYSK_MODE",
            match mode {
                BootMode::Normal => "normal",
                BootMode::Recovery => "recovery",
                BootMode::Charger => "charger",
            },
        )
        .env(
            "EGYSK_STAGE",
            script_name.split('.').next().unwrap_or(script_name),
        )
        .env(
            "EGYSK_PHASE",
            script_name.split_once('.').map_or("", |(_, phase)| phase),
        )
        .env("EGYSK_MODULE", &module.id)
        .env("MODDIR", directory)
        .env("EGYSK_MODULE_STATE", format!("{ROOT}/state/{}", module.id))
        .env("EGYSK_BACKING_ROOT", backing_root)
        .env(
            "PATH",
            format!("{ROOT}/rd/bin:{BIN}:/system/bin:/system/xbin:/vendor/bin"),
        );
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {} {script_name}", module.id))?;
    wait_until(&mut child, deadline, kill_descendants)
}
