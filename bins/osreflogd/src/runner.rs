//! Running a probe: `osreflogd --probe-runner '<spec>'`, started by the daemon as its own
//! transient systemd unit (`PrivateMounts=yes`, its own memory limit). It mounts the btrfs
//! top level privately to reach the probe, runs the test in nspawn or in a VM booting the
//! probe's own kernel, and prints a [`RunResult`] as JSON.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub use osreflog_bisect::Tier;
use osreflog_bisect::Verdict;
use rustix::mount::{MountFlags, UnmountFlags, mount, unmount};
use serde::{Deserialize, Serialize};

use crate::layout::would_propagate;
use crate::probe::{PROBE_DIR, newest_kernel};

pub const FLAG: &str = "--probe-runner";
const RUN_DIR: &str = "/run/osreflog-probe";
/// Free memory a VM probe needs beyond its guest RAM before it starts.
const VM_HEADROOM_MIB: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    /// The btrfs device (`/dev/sda3`).
    pub device: String,
    /// The probe's path from the top level (`osreflog/tmp/probe-3`).
    pub probe: String,
    pub tier: Tier,
    /// nspawn: boot systemd in the container instead of running the test as a command.
    pub boot: bool,
    pub timeout_s: u64,
    pub mem_mib: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    pub verdict: Option<Verdict>,
    pub exit_code: Option<i32>,
    /// From starting nspawn or qemu to its exit.
    pub ms: u64,
    /// qemu's peak resident memory (vm tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_rss_kib: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Time spent building the probe kernel's initramfs, if it wasn't cached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initrd_ms: Option<u64>,
    /// Why there's no verdict, or anything worth knowing about the run.
    pub detail: String,
    /// The end of the test's output.
    pub output: String,
}

fn ms_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn tail(text: &str, lines: usize) -> String {
    let v: Vec<&str> = text.lines().collect();
    v[v.len().saturating_sub(lines)..].join("\n")
}

fn mem_available_mib() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find_map(|l| l.strip_prefix("MemAvailable:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        })
        .map_or(0, |kib| kib / 1024)
}

fn vm_hwm_kib(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Waits for `child` up to `timeout`, sampling its peak RSS. `None`: killed at the timeout.
fn wait(child: &mut Child, timeout: Duration) -> (Option<std::process::ExitStatus>, Option<u64>) {
    let deadline = Instant::now() + timeout;
    let mut peak = None;
    loop {
        if let Some(h) = vm_hwm_kib(child.id()) {
            peak = Some(peak.map_or(h, |p: u64| p.max(h)));
        }
        match child.try_wait() {
            Ok(Some(status)) => return (Some(status), peak),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (None, peak);
            }
        }
    }
}

/// The exit code the probe unit recorded, if it got that far.
fn recorded(probe: &Path) -> (Option<i32>, String) {
    let dir = probe.join(PROBE_DIR);
    let code = std::fs::read_to_string(dir.join("result"))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let output = std::fs::read_to_string(dir.join("output")).unwrap_or_default();
    (code, tail(&output, 20))
}

fn nspawn(probe: &Path, spec: &Spec, out: &mut RunResult) {
    let mut cmd = Command::new("systemd-nspawn");
    cmd.args([
        "-q",
        "--register=no",
        "--keep-unit",
        "--private-network",
        "-D",
    ])
    .arg(probe)
    .stdin(Stdio::null());
    let log = probe.join(PROBE_DIR).join("nspawn.log");
    let Ok(log_file) = std::fs::File::create(&log) else {
        out.detail = "can't write into the probe".into();
        return;
    };
    let err_file = log_file.try_clone().ok();
    cmd.stdout(log_file);
    if let Some(e) = err_file {
        cmd.stderr(e);
    }
    if spec.boot {
        cmd.args(["-b", "--", "systemd.unit=osreflog-probe.target"]);
    } else {
        cmd.args(["--", "/bin/sh", "-c"]).arg(format!(
            "/{PROBE_DIR}/test > /{PROBE_DIR}/output 2>&1; echo $? > /{PROBE_DIR}/result"
        ));
    }
    let started = Instant::now();
    let Ok(mut child) = cmd.spawn() else {
        out.detail = "systemd-nspawn didn't start".into();
        return;
    };
    let (status, _) = wait(&mut child, Duration::from_secs(spec.timeout_s));
    out.ms = ms_since(started);
    if status.is_none() {
        out.detail = format!("timed out after {} s", spec.timeout_s);
    }
    let (code, output) = recorded(probe);
    out.exit_code = code;
    out.output = output;
    if code.is_none() && out.detail.is_empty() {
        out.detail = format!(
            "no result: {}",
            tail(&std::fs::read_to_string(&log).unwrap_or_default(), 5)
        );
    }
}

/// The probe kernel's virtiofs initramfs, built once per kernel version and cached.
fn initramfs(cache: &Path, probe: &Path, kv: &str, out: &mut RunResult) -> Option<PathBuf> {
    let img = cache.join(format!("initrd-virtiofs-{kv}.img"));
    if img.is_file() {
        return Some(img);
    }
    let _ = std::fs::create_dir_all(cache);
    let tmp = cache.join(format!(".initrd-{kv}.tmp"));
    let started = Instant::now();
    let mut cmd = Command::new("dracut");
    cmd.args([
        "-q",
        "--hostonly",
        "--no-hostonly-cmdline",
        "--omit",
        "plymouth",
    ])
    .args([
        "--add",
        "virtiofs",
        "--add-drivers",
        "virtiofs virtio_pci virtio_ring",
    ])
    .args(["--kver", kv, "-f"])
    .arg(&tmp)
    .stdin(Stdio::null())
    .stdout(Stdio::null());
    // A kernel this machine no longer has: collect its modules from the probe.
    if !Path::new("/usr/lib/modules").join(kv).is_dir() {
        cmd.arg("--sysroot").arg(probe);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => {
            out.initrd_ms = Some(ms_since(started));
            std::fs::rename(&tmp, &img).ok()?;
            Some(img)
        }
        Ok(o) => {
            out.detail = format!(
                "dracut failed for {kv}: {}",
                tail(&String::from_utf8_lossy(&o.stderr), 3)
            );
            None
        }
        Err(e) => {
            out.detail = format!("dracut: {e}");
            None
        }
    }
}

fn vm(probe: &Path, cache: &Path, run: &Path, spec: &Spec, out: &mut RunResult) {
    let Some(kv) = newest_kernel(probe) else {
        out.detail = "the probe has no kernel in /usr/lib/modules".into();
        return;
    };
    out.kernel = Some(kv.clone());
    let Some(initrd) = initramfs(cache, probe, &kv, out) else {
        return;
    };
    // The memory guard: one VM at a time, and only with room for it.
    let need = spec.mem_mib + VM_HEADROOM_MIB;
    let waited = Instant::now();
    while mem_available_mib() < need {
        if waited.elapsed() > Duration::from_secs(60) {
            out.detail = format!(
                "not enough free memory: {} MiB available, {need} MiB needed",
                mem_available_mib()
            );
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    let sock = run.join("vfs.sock");
    let Ok(mut vfs) = Command::new("/usr/libexec/virtiofsd")
        .arg(format!("--socket-path={}", sock.display()))
        .arg(format!("--shared-dir={}", probe.display()))
        .args(["--cache=auto", "--xattr"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        out.detail = "virtiofsd didn't start".into();
        return;
    };
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mem = format!("{}M", spec.mem_mib);
    let console = run.join("console.log");
    let started = Instant::now();
    let child = Command::new("/usr/bin/qemu-kvm")
        .args(["-M", "q35,accel=kvm", "-cpu", "host", "-smp", "2", "-m", &mem])
        .args([
            "-object",
            &format!("memory-backend-memfd,id=mem,size={mem},share=on"),
            "-numa",
            "node,memdev=mem",
        ])
        .args(["-device", "virtio-balloon-pci,free-page-reporting=on"])
        .arg("-kernel")
        .arg(probe.join("usr/lib/modules").join(&kv).join("vmlinuz"))
        .arg("-initrd")
        .arg(&initrd)
        .args([
            "-append",
            "root=virtiofs:rootfs rw console=ttyS0 selinux=0 systemd.unit=osreflog-probe.target panic=-1 rd.emergency=poweroff quiet",
        ])
        .args(["-chardev", &format!("socket,id=vfs,path={}", sock.display())])
        .args(["-device", "vhost-user-fs-pci,chardev=vfs,tag=rootfs"])
        .args(["-display", "none", "-monitor", "none", "-nic", "none", "-no-reboot"])
        .arg("-serial")
        .arg(format!("file:{}", console.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    match child {
        Ok(mut q) => {
            let (status, peak) = wait(&mut q, Duration::from_secs(spec.timeout_s));
            out.ms = ms_since(started);
            out.peak_rss_kib = peak;
            if status.is_none() {
                out.detail = format!("timed out after {} s", spec.timeout_s);
            }
        }
        Err(e) => out.detail = format!("qemu didn't start: {e}"),
    }
    let _ = vfs.kill();
    let _ = vfs.wait();
    let (code, output) = recorded(probe);
    out.exit_code = code;
    out.output = output;
    if code.is_none() && out.detail.is_empty() {
        out.detail = format!(
            "no result; the console ended with: {}",
            tail(&std::fs::read_to_string(&console).unwrap_or_default(), 5)
        );
    }
}

fn run(spec: &Spec) -> RunResult {
    let mut out = RunResult::default();
    let run = PathBuf::from(RUN_DIR).join(std::process::id().to_string());
    let top = run.join("top");
    if let Err(e) = std::fs::create_dir_all(&top) {
        out.detail = format!("creating {}: {e}", top.display());
        return out;
    }
    match would_propagate(&top) {
        Ok(false) => {}
        Ok(true) => {
            out.detail =
                "refusing to mount where it would propagate (run me with PrivateMounts=yes)".into();
            return out;
        }
        Err(e) => {
            out.detail = e.to_string();
            return out;
        }
    }
    if let Err(e) = mount(
        spec.device.as_str(),
        &top,
        "btrfs",
        MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOATIME,
        c"subvolid=5",
    ) {
        out.detail = format!("mounting the top level of {}: {e}", spec.device);
        return out;
    }
    let probe = top.join(&spec.probe);
    match spec.tier {
        Tier::Nspawn => nspawn(&probe, spec, &mut out),
        Tier::Vm => vm(&probe, &top.join("osreflog/cache"), &run, spec, &mut out),
    }
    out.verdict = out.exit_code.map(Verdict::from_exit);
    let _ = unmount(&top, UnmountFlags::DETACH);
    let _ = std::fs::remove_dir_all(&run);
    out
}

/// The runner's entry point: the spec as JSON in the first argument, the result as JSON on
/// stdout. Exit 0 whenever a result was printed (the verdict is inside it).
#[must_use]
pub fn runner_main(spec: &str) -> i32 {
    let Ok(spec) = serde_json::from_str::<Spec>(spec) else {
        eprintln!("bad probe spec");
        return 2;
    };
    let result = run(&spec);
    match serde_json::to_string(&result) {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(_) => 1,
    }
}
