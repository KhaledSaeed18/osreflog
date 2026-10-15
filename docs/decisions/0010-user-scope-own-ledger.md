# 0010: User scope: an agent per user, with the user's own ledger

- Status: Accepted
- Date: 2026-10-15

## Context
P4d adds the user scope: GNOME settings changes (dconf, through GSettings), credited to the app or command that made them. Something running in the user's session has to watch them, because dconf writes go over the **session** bus, which the system daemon can't see. The open question was where those records go. The plan had said "reported to the daemon".

## Decision
`osreflog-user`, a systemd user service, monitors `ca.desrt.dconf.Writer` traffic on the session bus. It pairs each `Change` call with its reply and the `Notify` signal through dconf's tag, and stores changes in **the user's own ledger**, `~/.local/state/osreflog/user.db` in a `0700` directory. It uses the same schema and hash chain as the system ledger. `osreflog user [KEY]` reads that ledger directly, without the daemon.

## Alternatives considered
- **Report to osreflogd over the system bus.** One timeline, but:
  - **Privacy:** the `…read` polkit action is open to any active local session, so one user's settings history would be readable by the next person to log in at the seat.
  - **Integrity:** the daemon can't verify anything a user agent reports. It could only label the records "claimed by uid N", so they'd carry no more weight than in the user's own file.
  - **Attack surface:** a new write method on a root daemon, callable by every user, with rate limits and storage quotas to get right.
- **Read the dconf database file (GVDB) with inode marks.** Gives values without the bus, but not who asked; attributing the writer is the point.

## Consequences
- The system and user histories are separate. A future `osreflog log --all` can merge them in the client, which already has both.
- The user ledger is as trustworthy as the user's account: anything running as that user can rewrite it. It's a record for the user, not evidence for the admin.
- Each user who wants it enables the agent: `systemctl --user enable --now osreflog-user`.
