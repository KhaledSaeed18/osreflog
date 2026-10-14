# 0008: D-Bus system service + polkit

- Status: Accepted
- Date: 2026-10-14

## Context
The CLI, the GNOME app and the dnf hook all talk to a root daemon. Privileged actions need per-action authorization that works with GNOME's auth dialogs.

## Decision
A D-Bus system service (`io.github.khaledsaeed18.osreflog`, zbus) with polkit actions `…read`, `…checkpoint`, `…undo`, `…bisect`, `…bisect-metal`. The dnf hook calls the CLI, which talks D-Bus.

## Alternatives considered
- **Varlink (systemd's IPC):** modern and well suited to a CLI, but the GNOME app and polkit integration are more natural on D-Bus, and drainscope already uses zbus.
- **A Unix socket with a custom protocol:** we'd have to reinvent auth.

## Consequences
- The D-Bus interface is the stable API; the CLI is porcelain over it, with `--json` for scripting.
