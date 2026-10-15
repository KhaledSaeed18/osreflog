//! The sandbox the summarizing worker enters before it reads any input (threat model A2: the
//! parsers read content any local user may influence). In order:
//!
//! 1. resource limits: 512 MiB address space, 5 s CPU, no file writes, no new processes
//! 2. drop every capability (the worker is spawned by the root daemon)
//! 3. no-new-privileges
//! 4. Landlock with every filesystem and network right handled and none granted: the worker
//!    can't open, create or connect anything; signals and abstract sockets are scoped too
//! 5. a seccomp filter that kills the process on any system call outside a short allowlist
//!
//! What's left: read stdin, write stdout, allocate memory, exit.
//!
//! The undo worker gets a looser sandbox ([`enter_undo`]): it has to write files.

use std::io;

use rustix::thread::{CapabilitySet, CapabilitySets, set_capabilities, set_no_new_privs};

const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

fn check(ret: libc::c_long) -> io::Result<libc::c_long> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn limits() -> io::Result<()> {
    for (resource, value) in [
        (libc::RLIMIT_AS, 512u64 << 20),
        (libc::RLIMIT_CPU, 5),
        (libc::RLIMIT_FSIZE, 0),
        (libc::RLIMIT_NPROC, 0),
        (libc::RLIMIT_NOFILE, 8),
        (libc::RLIMIT_CORE, 0),
    ] {
        let lim = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: plain syscall with a valid, initialized struct.
        if unsafe { libc::setrlimit(resource, &raw const lim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The Landlock ABI version, or an error if Landlock is unavailable.
fn landlock_abi() -> io::Result<i64> {
    // SAFETY: the documented way to query the ABI: null attr, size 0, VERSION flag.
    let v = check(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    })?;
    Ok(i64::from(i32::try_from(v).unwrap_or(0)))
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

const LANDLOCK_RULE_PATH_BENEATH: libc::c_uint = 1;

/// Every filesystem right this kernel's Landlock knows: v1 13 bits, v2 +REFER, v3 +TRUNCATE,
/// v5 +`IOCTL_DEV`.
fn fs_rights(abi: i64) -> u64 {
    let mut fs: u64 = (1 << 13) - 1;
    if abi >= 2 {
        fs |= 1 << 13;
    }
    if abi >= 3 {
        fs |= 1 << 14;
    }
    if abi >= 5 {
        fs |= 1 << 15;
    }
    fs
}

/// Restricts this process to the given directories (every filesystem right beneath each),
/// with every other filesystem and network right handled and denied.
fn landlock(beneath: &[std::os::fd::BorrowedFd<'_>]) -> io::Result<i64> {
    use std::os::fd::AsRawFd as _;
    let abi = landlock_abi()?;
    let fs = fs_rights(abi);
    let attr = RulesetAttr {
        handled_access_fs: fs,
        handled_access_net: if abi >= 4 { 0b11 } else { 0 },
        scoped: if abi >= 6 { 0b11 } else { 0 },
    };
    let size = match abi {
        ..=3 => 8usize,
        4 | 5 => 16,
        _ => size_of::<RulesetAttr>(),
    };
    // SAFETY: `attr` is a valid `landlock_ruleset_attr` prefix of `size` bytes.
    let fd = check(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &raw const attr,
            size,
            0u32,
        )
    })?;
    let fd = i32::try_from(fd).map_err(|_| io::Error::other("bad ruleset fd"))?;
    let mut result = Ok(());
    for dir in beneath {
        let rule = PathBeneathAttr {
            allowed_access: fs,
            parent_fd: dir.as_raw_fd(),
        };
        // SAFETY: `rule` is a valid packed `landlock_path_beneath_attr`; `fd` is our ruleset.
        if let Err(e) = check(unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                fd,
                LANDLOCK_RULE_PATH_BENEATH,
                &raw const rule,
                0u32,
            )
        }) {
            result = Err(e);
            break;
        }
    }
    let restricted = result.and_then(|()| {
        // SAFETY: `fd` is the ruleset we just built.
        check(unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd, 0u32) }).map(|_| ())
    });
    // SAFETY: closing the fd we own.
    unsafe { libc::close(fd) };
    restricted?;
    Ok(abi)
}

#[cfg(target_arch = "x86_64")]
// BPF opcodes are small constants that fit a u16 field.
#[allow(clippy::cast_possible_truncation)]
fn seccomp() -> io::Result<()> {
    const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
    const X32_BIT: u32 = 0x4000_0000;
    let allowed: &[libc::c_long] = &[
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_mprotect,
        libc::SYS_madvise,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigaction,
        libc::SYS_sigaltstack,
        libc::SYS_futex,
        libc::SYS_getrandom,
        libc::SYS_clock_gettime,
        libc::SYS_sched_yield,
        // abort() on a panic: raise(SIGABRT) to itself (Landlock scopes signals to this domain).
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_tgkill,
    ];
    let stmt = |code: u16, k: u32| libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    };
    let jump = |code: u16, k: u32, jt: u8, jf: u8| libc::sock_filter { code, jt, jf, k };
    let ld_abs = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    let jeq = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    let jge = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
    let ret = (libc::BPF_RET | libc::BPF_K) as u16;
    let n = u8::try_from(allowed.len()).map_err(|_| io::Error::other("allowlist too long"))?;

    // seccomp_data: nr at offset 0, arch at offset 4.
    let mut prog = vec![
        stmt(ld_abs, 4),
        jump(jeq, AUDIT_ARCH_X86_64, 1, 0),
        stmt(ret, libc::SECCOMP_RET_KILL_PROCESS),
        stmt(ld_abs, 0),
        jump(jge, X32_BIT, 0, 1),
        stmt(ret, libc::SECCOMP_RET_KILL_PROCESS),
    ];
    for (i, nr) in allowed.iter().enumerate() {
        let i = u8::try_from(i).unwrap_or(u8::MAX);
        // On a match, jump past the remaining checks and the kill to the allow.
        prog.push(jump(jeq, u32::try_from(*nr).unwrap_or(u32::MAX), n - i, 0));
    }
    prog.push(stmt(ret, libc::SECCOMP_RET_KILL_PROCESS));
    prog.push(stmt(ret, libc::SECCOMP_RET_ALLOW));

    let fprog = libc::sock_fprog {
        len: u16::try_from(prog.len()).unwrap_or(u16::MAX),
        filter: prog.as_mut_ptr(),
    };
    // SAFETY: `fprog` points at `prog`, which outlives the call; no-new-privileges is set.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &raw const fprog,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
fn seccomp() -> io::Result<()> {
    Err(io::Error::other(
        "the seccomp allowlist is only defined for x86_64",
    ))
}

/// Enters the sandbox. Fails closed: any step that fails is an error, and the worker then
/// exits without reading its input.
///
/// # Errors
/// If any step can't be applied (e.g. Landlock unavailable).
pub fn enter() -> io::Result<i64> {
    limits()?;
    let none = CapabilitySet::empty();
    set_capabilities(
        None,
        CapabilitySets {
            effective: none,
            permitted: none,
            inheritable: none,
        },
    )?;
    set_no_new_privs(true)?;
    let abi = landlock(&[])?;
    seccomp()?;
    Ok(abi)
}

/// Enters the undo worker's sandbox: limits, only `CAP_CHOWN`, `CAP_FOWNER` and
/// `CAP_DAC_OVERRIDE` (owner, mode, labels and files root can't otherwise write), no new
/// privileges, and Landlock access only beneath `writable` (the scope directories and the
/// attic, opened by the caller). No seccomp filter: the engine needs the whole `*at` family.
///
/// # Errors
/// If any step can't be applied.
pub fn enter_undo(writable: &[std::os::fd::BorrowedFd<'_>]) -> io::Result<i64> {
    for (resource, value) in [
        (libc::RLIMIT_AS, 1u64 << 30),
        (libc::RLIMIT_CPU, 60),
        (libc::RLIMIT_NPROC, 0),
        (libc::RLIMIT_NOFILE, 64),
        (libc::RLIMIT_CORE, 0),
    ] {
        let lim = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: plain syscall with a valid, initialized struct.
        if unsafe { libc::setrlimit(resource, &raw const lim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let keep = CapabilitySet::CHOWN | CapabilitySet::FOWNER | CapabilitySet::DAC_OVERRIDE;
    set_capabilities(
        None,
        CapabilitySets {
            effective: keep,
            permitted: keep,
            inheritable: CapabilitySet::empty(),
        },
    )?;
    set_no_new_privs(true)?;
    landlock(writable)
}
