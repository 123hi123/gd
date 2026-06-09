use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const FAN_CLASS_NOTIF: u32 = 0x0000_0000;
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
const FAN_REPORT_NAME: u32 = 0x0000_0800;
const FAN_MARK_ADD: u32 = 0x0000_0001;
const FAN_MARK_FILESYSTEM: u32 = 0x0000_0100;
const FAN_MARK_MOUNT: u32 = 0x0000_0010;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_RENAME: u64 = 0x1000_0000;
const FAN_ONDIR: u64 = 0x4000_0000;

const EVENT_METADATA_LEN: usize = 24;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_OLD_DFID_NAME: u8 = 10;
const FAN_EVENT_INFO_TYPE_NEW_DFID_NAME: u8 = 12;

pub enum DirEvent {
    Created(PathBuf),
    Deleted(PathBuf),
    Renamed(PathBuf, PathBuf),
}

pub fn init() -> io::Result<i32> {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_fanotify_init,
            FAN_CLASS_NOTIF | FAN_REPORT_DIR_FID | FAN_REPORT_NAME,
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    } as i32;

    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    let high_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 512) };
    if high_fd >= 0 {
        unsafe { libc::close(fd) };
        Ok(high_fd)
    } else {
        Ok(fd)
    }
}

pub fn open_mount_fd(path: &Path) -> io::Result<i32> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

pub fn mark_filesystem(fd: i32, path: &Path) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let mask = FAN_CREATE | FAN_DELETE | FAN_RENAME | FAN_ONDIR;

    let ret = unsafe {
        let r = libc::syscall(
            libc::SYS_fanotify_mark,
            fd,
            FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
            mask,
            libc::AT_FDCWD,
            c_path.as_ptr(),
        ) as i32;
        if r < 0 {
            libc::syscall(
                libc::SYS_fanotify_mark,
                fd,
                FAN_MARK_ADD | FAN_MARK_MOUNT,
                mask,
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

pub fn read_events(fd: i32, mount_fd: i32) -> Vec<DirEvent> {
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };

    let mut events = Vec::new();
    let mut buf = [0u8; 16384];

    loop {
        let n = unsafe {
            libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
        };
        if n <= 0 {
            break;
        }
        let n = n as usize;

        let mut offset = 0;
        while offset + EVENT_METADATA_LEN <= n {
            let event_len = u32::from_ne_bytes(
                buf[offset..offset + 4].try_into().unwrap(),
            ) as usize;
            if event_len < EVENT_METADATA_LEN || offset + event_len > n {
                break;
            }

            let mask = u64::from_ne_bytes(
                buf[offset + 8..offset + 16].try_into().unwrap(),
            );

            if mask & FAN_ONDIR != 0 {
                let info_start = offset + EVENT_METADATA_LEN;
                let info_end = offset + event_len;
                let info_buf = &buf[info_start..info_end];

                if mask & FAN_RENAME != 0 {
                    // A rename carries two info records in one event:
                    // OLD_DFID_NAME (source) and NEW_DFID_NAME (destination).
                    let mut old_path = None;
                    let mut new_path = None;
                    let mut p = 0;
                    while p + 4 <= info_buf.len() {
                        let rec_type = info_buf[p];
                        let rec_len = u16::from_ne_bytes(
                            info_buf[p + 2..p + 4].try_into().unwrap(),
                        ) as usize;
                        if rec_len < 4 || p + rec_len > info_buf.len() {
                            break;
                        }
                        let rec = &info_buf[p..p + rec_len];
                        if rec_type == FAN_EVENT_INFO_TYPE_OLD_DFID_NAME {
                            old_path = parse_dfid_name(rec, mount_fd);
                        } else if rec_type == FAN_EVENT_INFO_TYPE_NEW_DFID_NAME {
                            new_path = parse_dfid_name(rec, mount_fd);
                        }
                        p += rec_len;
                    }
                    match (old_path, new_path) {
                        (Some(o), Some(n)) => events.push(DirEvent::Renamed(o, n)),
                        // Only one side is inside the watched filesystem: a lone
                        // source is effectively a deletion, a lone destination a creation.
                        (Some(o), None) => events.push(DirEvent::Deleted(o)),
                        (None, Some(n)) => events.push(DirEvent::Created(n)),
                        (None, None) => {}
                    }
                } else if let Some(path) = parse_dfid_name(info_buf, mount_fd) {
                    if mask & FAN_CREATE != 0 {
                        events.push(DirEvent::Created(path));
                    } else if mask & FAN_DELETE != 0 {
                        events.push(DirEvent::Deleted(path));
                    }
                }
            }

            offset += event_len;
        }
    }

    unsafe { libc::fcntl(fd, libc::F_SETFL, 0) };
    events
}

fn parse_dfid_name(info_buf: &[u8], mount_fd: i32) -> Option<PathBuf> {
    if info_buf.len() < 20 {
        return None;
    }

    let info_type = info_buf[0];
    let info_len = u16::from_ne_bytes(info_buf[2..4].try_into().unwrap()) as usize;

    if info_type != FAN_EVENT_INFO_TYPE_DFID_NAME
        && info_type != FAN_EVENT_INFO_TYPE_OLD_DFID_NAME
        && info_type != FAN_EVENT_INFO_TYPE_NEW_DFID_NAME
    {
        return None;
    }
    if info_len > info_buf.len() {
        return None;
    }

    let fh_offset = 12;
    let handle_bytes = u32::from_ne_bytes(
        info_buf[fh_offset..fh_offset + 4].try_into().unwrap(),
    ) as usize;

    let fh_end = fh_offset + 8 + handle_bytes;
    if fh_end >= info_len {
        return None;
    }

    let dir_fd = unsafe {
        libc::syscall(
            libc::SYS_open_by_handle_at,
            mount_fd,
            info_buf[fh_offset..].as_ptr(),
            libc::O_RDONLY | libc::O_PATH,
        )
    } as i32;

    if dir_fd < 0 {
        return None;
    }

    let parent_path = std::fs::read_link(format!("/proc/self/fd/{dir_fd}")).ok();
    unsafe { libc::close(dir_fd) };
    let parent_path = parent_path?;

    let name_start = fh_end;
    let name_region = &info_buf[name_start..info_len];
    let name_end = name_region.iter().position(|&b| b == 0).unwrap_or(name_region.len());
    if name_end == 0 {
        return None;
    }

    let name = std::str::from_utf8(&name_region[..name_end]).ok()?;
    Some(parent_path.join(name))
}
