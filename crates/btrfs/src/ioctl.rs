//! The btrfs ioctls osreflog needs: snapshot, delete, subvolume info, send.
//!
//! This is the only module in osreflog that uses `unsafe`. The struct layouts mirror
//! `/usr/include/linux/btrfs.h`; sizes and opcodes are checked at compile time against the
//! values a C program printed for the same header (see docs/journal/p2-slice.md).

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::thread::JoinHandle;

use rustix::ioctl::{Getter, Opcode, Setter, ioctl, opcode};

const MAGIC: u8 = 0x94;
const SUBVOL_NAME_MAX: usize = 4039;
const SUBVOL_RDONLY: u64 = 1 << 1;
const SEND_FLAG_NO_FILE_DATA: u64 = 0x1;
const SEND_FLAG_VERSION: u64 = 0x8;

#[repr(C)]
struct VolArgsV2 {
    fd: i64,
    transid: u64,
    flags: u64,
    unused: [u64; 4],
    name: [u8; SUBVOL_NAME_MAX + 1],
}

#[repr(C)]
struct SendArgs {
    send_fd: i64,
    clone_sources_count: u64,
    clone_sources: *const u64,
    parent_root: u64,
    flags: u64,
    version: u32,
    reserved: [u8; 28],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Timespec {
    sec: u64,
    nsec: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SubvolInfoArgs {
    treeid: u64,
    name: [u8; 256],
    parent_id: u64,
    dirid: u64,
    generation: u64,
    flags: u64,
    uuid: [u8; 16],
    parent_uuid: [u8; 16],
    received_uuid: [u8; 16],
    ctransid: u64,
    otransid: u64,
    stransid: u64,
    rtransid: u64,
    ctime: Timespec,
    otime: Timespec,
    stime: Timespec,
    rtime: Timespec,
    reserved: [u64; 8],
}

const SNAP_CREATE_V2: Opcode = opcode::write::<VolArgsV2>(MAGIC, 23);
const SUBVOL_CREATE_V2: Opcode = opcode::write::<VolArgsV2>(MAGIC, 24);
const SEND: Opcode = opcode::write::<SendArgs>(MAGIC, 38);
const GET_SUBVOL_INFO: Opcode = opcode::read::<SubvolInfoArgs>(MAGIC, 60);
const SNAP_DESTROY_V2: Opcode = opcode::write::<VolArgsV2>(MAGIC, 63);

const _: () = {
    assert!(size_of::<VolArgsV2>() == 4096);
    assert!(size_of::<SendArgs>() == 72);
    assert!(size_of::<SubvolInfoArgs>() == 504);
    assert!(std::mem::offset_of!(SubvolInfoArgs, generation) == 280);
    assert!(std::mem::offset_of!(SubvolInfoArgs, otime) == 392);
    assert!(SNAP_CREATE_V2 as u64 == 0x5000_9417);
    assert!(SUBVOL_CREATE_V2 as u64 == 0x5000_9418);
    assert!(SNAP_DESTROY_V2 as u64 == 0x5000_943f);
    assert!(SEND as u64 == 0x4048_9426);
    assert!(GET_SUBVOL_INFO as u64 == 0x81f8_943c);
};

fn vol_args(name: &str) -> io::Result<VolArgsV2> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > SUBVOL_NAME_MAX
        || name == "."
        || name == ".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad subvolume name {name:?}"),
        ));
    }
    let mut args = VolArgsV2 {
        fd: 0,
        transid: 0,
        flags: 0,
        unused: [0; 4],
        name: [0; SUBVOL_NAME_MAX + 1],
    };
    args.name[..bytes.len()].copy_from_slice(bytes);
    Ok(args)
}

/// Snapshots the subvolume open at `source` as `dest_dir/name`.
///
/// # Errors
/// On a bad name or any ioctl failure (`EEXIST`, `EPERM`, `EXDEV`, ...).
pub fn create_snapshot(
    source: BorrowedFd<'_>,
    dest_dir: BorrowedFd<'_>,
    name: &str,
    readonly: bool,
) -> io::Result<()> {
    let mut args = vol_args(name)?;
    args.fd = i64::from(source.as_raw_fd());
    args.flags = if readonly { SUBVOL_RDONLY } else { 0 };
    // SAFETY: SNAP_CREATE_V2 takes a `btrfs_ioctl_vol_args_v2`, which `VolArgsV2` mirrors
    // (size and offsets asserted above); the kernel only reads it.
    unsafe { ioctl(dest_dir, Setter::<SNAP_CREATE_V2, VolArgsV2>::new(args)) }?;
    Ok(())
}

/// Creates an empty subvolume `parent_dir/name`.
///
/// # Errors
/// On a bad name or any ioctl failure.
pub fn create_subvolume(parent_dir: BorrowedFd<'_>, name: &str) -> io::Result<()> {
    let args = vol_args(name)?;
    // SAFETY: SUBVOL_CREATE_V2 reads a `btrfs_ioctl_vol_args_v2`, as `create_snapshot` does.
    unsafe { ioctl(parent_dir, Setter::<SUBVOL_CREATE_V2, VolArgsV2>::new(args)) }?;
    Ok(())
}

/// Deletes the subvolume `parent_dir/name` (it must not contain other subvolumes).
///
/// # Errors
/// On a bad name or any ioctl failure.
pub fn delete_subvolume(parent_dir: BorrowedFd<'_>, name: &str) -> io::Result<()> {
    let args = vol_args(name)?;
    // SAFETY: as for `create_snapshot`: SNAP_DESTROY_V2 reads a `btrfs_ioctl_vol_args_v2`.
    unsafe { ioctl(parent_dir, Setter::<SNAP_DESTROY_V2, VolArgsV2>::new(args)) }?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubvolumeInfo {
    pub treeid: u64,
    pub generation: u64,
    pub ctransid: u64,
    pub otransid: u64,
    pub uuid: [u8; 16],
    pub parent_uuid: [u8; 16],
    pub readonly: bool,
    /// Creation time, seconds since the epoch.
    pub otime: u64,
}

/// Information about the subvolume containing the open directory `fd`.
///
/// # Errors
/// If `fd` is not on btrfs, or on any ioctl failure.
pub fn subvolume_info(fd: BorrowedFd<'_>) -> io::Result<SubvolumeInfo> {
    // SAFETY: GET_SUBVOL_INFO fills a `btrfs_ioctl_get_subvol_info_args`, which
    // `SubvolInfoArgs` mirrors (size and offsets asserted above); all fields are plain data,
    // so any bit pattern the kernel writes is valid.
    let a = unsafe { ioctl(fd, Getter::<GET_SUBVOL_INFO, SubvolInfoArgs>::new()) }?;
    Ok(SubvolumeInfo {
        treeid: a.treeid,
        generation: a.generation,
        ctransid: a.ctransid,
        otransid: a.otransid,
        uuid: a.uuid,
        parent_uuid: a.parent_uuid,
        readonly: a.flags & SUBVOL_RDONLY != 0,
        otime: a.otime.sec,
    })
}

/// A send stream being produced by the kernel. Read it to the end, then call
/// [`SendStream::finish`] for the ioctl's result. Dropping it early stops the send.
pub struct SendStream {
    reader: Option<File>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Read for SendStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.reader {
            Some(r) => r.read(buf),
            None => Ok(0),
        }
    }
}

impl SendStream {
    /// Waits for the kernel to finish and returns its result.
    ///
    /// # Errors
    /// The send ioctl's error, if it failed.
    pub fn finish(mut self) -> io::Result<()> {
        self.reader.take();
        self.join()
    }

    fn join(&mut self) -> io::Result<()> {
        match self.worker.take() {
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("send thread panicked"))),
            None => Ok(()),
        }
    }
}

impl Drop for SendStream {
    fn drop(&mut self) {
        // Closing the read end makes the kernel's writes fail with EPIPE, ending the ioctl.
        self.reader.take();
        let _ = self.join();
    }
}

/// Starts `btrfs send` of the read-only subvolume open at `subvol`, incremental against the
/// subvolume with tree id `parent` if given (it must be read-only and on the same
/// filesystem). `no_data` sends `update_extent` instead of file contents. Protocol v2.
///
/// # Errors
/// If the pipe or the thread can't be created. Ioctl errors come from
/// [`SendStream::finish`].
pub fn send(subvol: OwnedFd, parent: Option<u64>, no_data: bool) -> io::Result<SendStream> {
    let (read_end, write_end) = rustix::pipe::pipe()?;
    let worker = std::thread::Builder::new()
        .name("btrfs-send".into())
        .spawn(move || {
            let clone_sources: Vec<u64> = parent.into_iter().collect();
            let mut flags = SEND_FLAG_VERSION;
            if no_data {
                flags |= SEND_FLAG_NO_FILE_DATA;
            }
            let args = SendArgs {
                send_fd: i64::from(write_end.as_raw_fd()),
                clone_sources_count: clone_sources.len() as u64,
                clone_sources: if clone_sources.is_empty() {
                    std::ptr::null()
                } else {
                    clone_sources.as_ptr()
                },
                parent_root: parent.unwrap_or(0),
                flags,
                version: 2,
                reserved: [0; 28],
            };
            // SAFETY: SEND reads a `btrfs_ioctl_send_args`, which `SendArgs` mirrors (size
            // asserted above). `clone_sources` points into a Vec that outlives the call, and
            // `send_fd` is the pipe's write end, open until this closure returns.
            let result = unsafe { ioctl(subvol.as_fd(), Setter::<SEND, SendArgs>::new(args)) };
            drop(write_end);
            result.map_err(io::Error::from)
        })?;
    Ok(SendStream {
        reader: Some(File::from(read_end)),
        worker: Some(worker),
    })
}

#[cfg(test)]
mod tests {
    use super::vol_args;

    #[test]
    fn subvolume_names_are_validated() {
        assert!(vol_args("c000001").is_ok());
        for bad in ["", ".", "..", "a/b", "nul\0"] {
            assert!(vol_args(bad).is_err(), "{bad:?}");
        }
        assert!(vol_args(&"x".repeat(4040)).is_err());
        assert!(vol_args(&"x".repeat(4039)).is_ok());
    }
}
