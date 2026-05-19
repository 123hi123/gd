use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

// fanotify constants (not all exposed by libc/nix crates)
const FAN_CLASS_NOTIF: u32 = 0x0000_0000;
const FAN_REPORT_FID: u32 = 0x0000_0200;
const FAN_MARK_ADD: u32 = 0x0000_0001;
const FAN_MARK_FILESYSTEM: u32 = 0x0000_0100;
const FAN_MARK_MOUNT: u32 = 0x0000_0010;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_ONDIR: u64 = 0x4000_0000;

const EVENT_METADATA_LEN: usize = 24; // sizeof(fanotify_event_metadata)

pub fn init() -> io::Result<i32> {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_fanotify_init,
            FAN_CLASS_NOTIF | FAN_REPORT_FID,
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    } as i32;

    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // Move fd to a high number so jwalk/rayon don't accidentally close it
    let high_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 512) };
    if high_fd >= 0 {
        unsafe { libc::close(fd) };
        Ok(high_fd)
    } else {
        Ok(fd)
    }
}

pub fn mark_filesystem(fd: i32, path: &Path) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    // Try FAN_MARK_FILESYSTEM first, fall back to FAN_MARK_MOUNT
    let ret = unsafe {
        let r = libc::syscall(
            libc::SYS_fanotify_mark,
            fd,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            FAN_CREATE | FAN_DELETE | FAN_ONDIR,
            libc::AT_FDCWD,
            c_path.as_ptr(),
        ) as i32;
        if r < 0 {
            libc::syscall(
                libc::SYS_fanotify_mark,
                fd,
                FAN_MARK_ADD | FAN_MARK_MOUNT,
                FAN_CREATE | FAN_DELETE | FAN_ONDIR,
                libc::AT_FDCWD,
                c_path.as_ptr(),
            ) as i32
        } else {
            r
        }
    };

    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn poll_events(fd: i32, timeout_ms: i32) -> io::Result<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret > 0)
}

/// Drain all pending events without blocking.
/// With FAN_REPORT_FID, events have fd=-1 so nothing to close.
pub fn drain(fd: i32) -> io::Result<usize> {
    // Temporarily set nonblock to drain
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };

    let mut buf = [0u8; 8192];
    let mut count = 0usize;

    loop {
        let n = unsafe {
            libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
        };

        if n <= 0 {
            break;
        }

        let mut offset = 0;
        while offset + EVENT_METADATA_LEN <= n as usize {
            let event_len = u32::from_ne_bytes(
                buf[offset..offset + 4].try_into().unwrap(),
            ) as usize;

            let event_fd = i32::from_ne_bytes(
                buf[offset + 12..offset + 16].try_into().unwrap(),
            );

            if event_fd >= 0 {
                unsafe { libc::close(event_fd) };
            }

            count += 1;
            if event_len == 0 {
                break;
            }
            offset += event_len;
        }
    }

    // Restore blocking mode for poll
    unsafe { libc::fcntl(fd, libc::F_SETFL, 0) };

    Ok(count)
}
