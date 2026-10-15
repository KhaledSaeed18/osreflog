//! The fanotify system calls and `open_by_handle_at`: the only `unsafe` in this crate.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::event::Handle;

/// A notification group reporting directory file handles and names (`FAN_REPORT_DFID_NAME`),
/// non-blocking, with the kernel's default bounded queue (an overflow is reported as an event).
pub struct Group {
    fd: OwnedFd,
}

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

impl Group {
    /// # Errors
    /// Without `CAP_SYS_ADMIN`, or on kernels without FID reporting (< 5.9).
    pub fn new() -> io::Result<Self> {
        let flags = libc::FAN_CLASS_NOTIF
            | libc::FAN_CLOEXEC
            | libc::FAN_NONBLOCK
            | libc::FAN_REPORT_DFID_NAME;
        let event_flags = (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as libc::c_uint;
        // SAFETY: plain syscall with constant flags; the returned fd is owned from here on.
        let fd = check(unsafe { libc::fanotify_init(flags, event_flags) })?;
        // SAFETY: `fd` was just returned by the kernel and is not owned elsewhere.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// Marks the whole filesystem containing `path` for `mask`.
    ///
    /// # Errors
    /// If the filesystem doesn't support file handles, or the mark is refused.
    pub fn mark_filesystem(&self, path: &Path, mask: u64) -> io::Result<()> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let flags = libc::FAN_MARK_ADD | libc::FAN_MARK_FILESYSTEM;
        // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
        check(unsafe {
            libc::fanotify_mark(self.fd.as_raw_fd(), flags, mask, libc::AT_FDCWD, c.as_ptr())
        })?;
        Ok(())
    }

    /// Reads pending events into `buf`; `Ok(0)` when none are pending.
    ///
    /// # Errors
    /// On read errors other than `EAGAIN`.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `buf` is valid for writes of `buf.len()` bytes.
        let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::WouldBlock {
                Ok(0)
            } else {
                Err(e)
            };
        }
        Ok(n.unsigned_abs())
    }
}

impl AsFd for Group {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// The filesystem id fanotify reports for objects under `path` (btrfs: per subvolume).
///
/// # Errors
/// If `statfs` fails.
pub fn fsid_of(path: &Path) -> io::Result<[i32; 2]> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut st = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `c` is a valid path and `st` points to writable storage for one `statfs`.
    check(unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) })?;
    // SAFETY: statfs succeeded, so `st` is initialized.
    let st = unsafe { st.assume_init() };
    // SAFETY: `fsid_t` is two `c_int`s (8 bytes) with private fields; reinterpret as such.
    Ok(unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(st.f_fsid) })
}

/// Opens the object behind `handle` on the filesystem of `mount` (needs
/// `CAP_DAC_READ_SEARCH`). `flags` as for `open(2)`, e.g. `O_PATH | O_DIRECTORY`.
///
/// # Errors
/// `ESTALE` if the object is gone, or any other open error.
pub fn open_by_handle(
    mount: BorrowedFd<'_>,
    handle: &Handle,
    flags: libc::c_int,
) -> io::Result<OwnedFd> {
    // struct file_handle { u32 handle_bytes; i32 handle_type; unsigned char f_handle[]; },
    // built in u32 words so the header fields are aligned.
    let words = 2 + handle.bytes.len().div_ceil(4);
    let mut buf = vec![0u32; words];
    buf[0] = u32::try_from(handle.bytes.len())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    buf[1] = handle.handle_type.cast_unsigned();
    // SAFETY: `buf` has room for the 8-byte header plus the handle bytes; the copy stays in it.
    unsafe {
        std::ptr::copy_nonoverlapping(
            handle.bytes.as_ptr(),
            buf.as_mut_ptr().add(2).cast::<u8>(),
            handle.bytes.len(),
        );
    }
    // SAFETY: `buf` is a correctly laid out `file_handle` that outlives the call.
    let fd = check(unsafe {
        libc::open_by_handle_at(
            mount.as_raw_fd(),
            buf.as_mut_ptr().cast::<libc::file_handle>(),
            flags | libc::O_CLOEXEC,
        )
    })?;
    // SAFETY: the kernel returned a new fd that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
