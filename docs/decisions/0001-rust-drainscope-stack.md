# 0001: Rust, with drainscope's stack

- Status: Accepted; amended 2026-10-15 (P3): eBPF programs are C, as in drainscope
- Date: 2026-10-14

## Context
The operator knows Go and Rust. urljail is Go; drainscope is Rust with aya (eBPF), zbus (D-Bus) and rusqlite, and ships a GNOME UI. osreflog needs eBPF, D-Bus, SQLite, raw ioctls (btrfs, fanotify, FICLONE), `openat2`, a sandboxed parser, and later a GTK4 app.

## Decision
Rust for every userspace component. eBPF programs in C (CO-RE, `tp_btf`), compiled by clang in `build.rs` and loaded with aya, the same split drainscope uses. Kernel-side Rust (aya-ebpf) was the first plan; it needs a nightly toolchain and `bpf-linker`, too heavy on this machine's connection for programs of this size. Reuse drainscope's choices: **aya**, **zbus**, **rusqlite**, plus `rustix` for syscalls (`openat2`, `fanotify_*`, ioctls), `blake3`, `zstd`, `landlock`, `seccompiler`. Pin the toolchain in `rust-toolchain.toml`.

## Alternatives considered
- **Go (as urljail):** cilium/ebpf is excellent, but parsing untrusted file content benefits from Rust's memory safety, and there's no GTK4 story as good as gtk-rs.
- **libbpf-rs as the loader:** more examples online, but aya is what drainscope uses, and it's already in the cargo cache.

## Consequences
- Patterns from drainscope (D-Bus service shape, cgroup → app mapping, aya loading) carry over directly.
- The send-stream parser and btrfs ioctl bindings are written by hand. That's intended learning, and there's no maintained crate for the send stream.
