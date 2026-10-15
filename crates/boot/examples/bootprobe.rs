//! The boot crate by hand, for the boot gate's test VM (`scripts/p7/g2-step.sh`). Not part of
//! osreflog: the daemon calls the crate itself.
//!
//! ```text
//! bootprobe record RECORD.json
//! bootprobe prepare RECORD.json N SUBVOL PROBE_ROOT [--install]
//! bootprobe arm | verify RECORD.json
//! bootprobe leftovers | disarm
//! ```
//!
//! `BOOT_ROOT` (default `/boot`). `--install` copies the kernel to `/boot/osreflog/` and builds
//! its initramfs even when `/boot` already has the same image.

use std::path::PathBuf;
use std::process::ExitCode;

use osreflog_boot::{Boot, DefaultRecord, kernel};

fn main() -> ExitCode {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bootprobe: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read_record(path: &str) -> Result<DefaultRecord, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let boot = Boot::new(
        std::env::var_os("BOOT_ROOT").map_or_else(|| PathBuf::from("/boot"), PathBuf::from),
    );
    let arg = |i: usize| args.get(i).map(String::as_str).ok_or("missing argument");
    match arg(0)? {
        "record" => {
            let rec = boot.default_record()?;
            std::fs::write(arg(1)?, serde_json::to_string_pretty(&rec)?)?;
            println!("default {} ({})", rec.saved_entry, &rec.entry_blake3[..16]);
        }
        "prepare" => {
            let rec = read_record(arg(1)?)?;
            let n: u32 = arg(2)?.parse()?;
            let probe = PathBuf::from(arg(4)?);
            let mut plan = kernel::plan(&boot.root, &probe)?;
            if args.iter().any(|a| a == "--install")
                && let kernel::Plan::Reuse { version } = &plan
            {
                let vmlinuz = probe.join("usr/lib/modules").join(version).join("vmlinuz");
                plan = kernel::Plan::Install {
                    version: version.clone(),
                    vmlinuz,
                };
            }
            let started = std::time::Instant::now();
            kernel::install(&plan, &boot.root, &probe, std::path::Path::new("dracut"))?;
            let entry = boot.write_probe_entry(&rec, n, arg(3)?, &plan)?;
            println!(
                "kernel {} {} in {} ms; entry {}",
                plan.version(),
                if matches!(plan, kernel::Plan::Reuse { .. }) {
                    "reused"
                } else {
                    "installed"
                },
                started.elapsed().as_millis(),
                entry.display()
            );
        }
        "arm" => {
            boot.arm(&read_record(arg(1)?)?)?;
            println!("armed for the next boot");
        }
        "verify" => {
            boot.verify(&read_record(arg(1)?)?)?;
            println!("default untouched");
        }
        "leftovers" => println!("{}", serde_json::to_string(&boot.leftovers()?)?),
        "disarm" => println!("removed {}", serde_json::to_string(&boot.disarm()?)?),
        other => return Err(format!("unknown command {other}").into()),
    }
    Ok(())
}
