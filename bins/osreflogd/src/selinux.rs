//! SELinux domains for the workers (0.3.0): the summarizer and the undo worker ask, at exec,
//! for a domain of their own from the `osreflog-selinux` module (`setexeccon` through
//! `/proc/thread-self/attr/exec`, written in the child between fork and exec).
//!
//! Best effort, never in the way: without SELinux, or without the module (the context is
//! then invalid and the write fails), the worker runs as osreflogd does, as before. If the
//! policy refuses the transition, the exec fails; the spawn is tried again without it, and
//! osreflogd stops asking until it restarts.

use std::ffi::CString;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SUMMARY_DOMAIN: &str = "osreflog_summary_t";
pub const UNDO_DOMAIN: &str = "osreflog_undo_t";

/// Cleared when the policy refused a transition once.
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Said once in the journal: which domain the first worker of each kind ran in.
static TOLD: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// `current` with its type replaced by `domain`: `system_u:system_r:unconfined_service_t:s0`
/// becomes `system_u:system_r:osreflog_summary_t:s0`. `None` for anything not shaped like a
/// context.
#[must_use]
pub fn with_type(current: &str, domain: &str) -> Option<String> {
    let current = current.trim_end_matches(['\0', '\n']);
    let mut parts = current.splitn(4, ':');
    let (user, role, _ty, level) = (parts.next()?, parts.next()?, parts.next()?, parts.next());
    if user.is_empty() || role.is_empty() {
        return None;
    }
    Some(match level {
        Some(level) => format!("{user}:{role}:{domain}:{level}"),
        None => format!("{user}:{role}:{domain}"),
    })
}

fn context_for(domain: &str) -> Option<&'static CString> {
    static SUMMARY: OnceLock<Option<CString>> = OnceLock::new();
    static UNDO: OnceLock<Option<CString>> = OnceLock::new();
    let cell = if domain == UNDO_DOMAIN {
        &UNDO
    } else {
        &SUMMARY
    };
    cell.get_or_init(|| {
        let current = std::fs::read_to_string("/proc/self/attr/current").ok()?;
        CString::new(with_type(&current, domain)?).ok()
    })
    .as_ref()
}

/// Spawns `cmd` in `domain` when the policy has it, as osreflogd's own domain otherwise.
///
/// # Errors
/// As [`Command::spawn`], once without the domain.
pub fn spawn(cmd: &mut Command, domain: &str) -> io::Result<Child> {
    let Some(context) = context_for(domain) else {
        return cmd.spawn();
    };
    let bytes: &'static [u8] = context.as_bytes_with_nul();
    // SAFETY: between fork and exec only async-signal-safe calls (open, write, close) on
    // memory prepared before the fork (`context` is 'static) and an atomic load.
    unsafe {
        cmd.pre_exec(move || {
            if ENABLED.load(Ordering::Relaxed) {
                let fd = libc::open(
                    c"/proc/thread-self/attr/exec".as_ptr(),
                    libc::O_WRONLY | libc::O_CLOEXEC,
                );
                if fd >= 0 {
                    // EINVAL: no such domain (the module isn't installed): run as we are.
                    let _ = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                    libc::close(fd);
                }
            }
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Err(e)
            if e.kind() == io::ErrorKind::PermissionDenied && ENABLED.load(Ordering::Relaxed) =>
        {
            ENABLED.store(false, Ordering::Relaxed);
            tracing::warn!(
                "SELinux refused osreflogd's workers their domain ({domain}): {e}; they run as osreflogd until it restarts"
            );
            cmd.spawn()?
        }
        other => other?,
    };
    let told = &TOLD[usize::from(domain == UNDO_DOMAIN)];
    if !told.swap(true, Ordering::Relaxed) {
        let running = std::fs::read_to_string(format!("/proc/{}/attr/current", child.id()))
            .map(|c| c.trim_end_matches(['\0', '\n']).to_owned())
            .unwrap_or_default();
        if !running.is_empty() {
            tracing::info!("worker started as {running}");
        }
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_type_is_swapped_and_the_rest_kept() {
        assert_eq!(
            with_type(
                "system_u:system_r:unconfined_service_t:s0\0",
                SUMMARY_DOMAIN
            )
            .as_deref(),
            Some("system_u:system_r:osreflog_summary_t:s0")
        );
        assert_eq!(
            with_type(
                "unconfined_u:unconfined_r:unconfined_t:s0-s0:c0.c1023\n",
                UNDO_DOMAIN
            )
            .as_deref(),
            Some("unconfined_u:unconfined_r:osreflog_undo_t:s0-s0:c0.c1023")
        );
        assert_eq!(with_type("kernel", UNDO_DOMAIN), None);
    }

    #[test]
    fn without_the_module_the_worker_runs_anyway() {
        // As a user, with no osreflog_summary_t defined (or no SELinux at all): the write
        // fails and the child runs as its parent.
        let mut cmd = Command::new("true");
        let status = spawn(&mut cmd, SUMMARY_DOMAIN).unwrap().wait().unwrap();
        assert!(status.success());
    }
}
