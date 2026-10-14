# 0001: Rust, with drainscope's stack

- Status: Accepted
- Date: 2026-10-14

## Context
The operator knows Go and Rust. urljail is Go; drainscope is Rust with aya (eBPF), zbus (D-Bus) and rusqlite, and ships a GNOME UI. osreflog needs eBPF, D-Bus, SQLite, raw ioctls (btrfs, fanotify, FICLONE), `openat2`, a sandboxed parser, and later a GTK4 app.

## Decision
Rust for every component. eBPF programs in Rust via aya-ebpf. Reuse drainscope's choices: **aya**, **zbus**, **rusqlite**, plus `rustix` for syscalls (`openat2`, `fanotify_*`, ioctls), `blake3`, `zstd`, `landlock`, `seccompiler`. Pin the toolchain in `rust-toolchain.toml`.

## Alternatives considered
- **Go (as urljail):** cilium/ebpf is excellent, but parsing untrusted file content benefits from Rust's memory safety, and there's no GTK4 story as good as gtk-rs.
- **C for the eBPF part (libbpf-rs):** more examples online, but aya keeps one language and matches drainscope.

## Consequences
- Patterns from drainscope (D-Bus service shape, cgroup → app mapping, aya loading) carry over directly.
- The send-stream parser and btrfs ioctl bindings are written by hand. That's intended learning, and there's no maintained crate for the send stream.
